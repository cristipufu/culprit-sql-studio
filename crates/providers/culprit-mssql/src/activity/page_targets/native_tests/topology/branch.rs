//! A bounded fixture-only locator supplies an actual branch address; the shared header reader qualifies it.
use super::*;
use crate::activity::page_targets::rows::HeaderRecord;

const LOOKAHEAD: usize = 2;
// Documented cached-page coordinates join the exact IN_ROW allocation unit to the owned partition/HoBT.
// TOP bounds returned rows, not buffer-pool enumeration. This paid fixture query grants no source cost approval.
// https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-views/sys-dm-os-buffer-descriptors-transact-sql
// https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-allocation-units-transact-sql
const LOCATOR: &str = r#"SELECT TOP(@P1) /* culprit fixture exact owned branch locator */
 b.database_id,b.file_id,b.page_id,b.page_level,
 CONVERT(varchar(24),b.allocation_unit_id) AS allocation_unit_id,
 p.object_id,p.index_id,p.partition_number,i.type AS index_type,
 CONVERT(varchar(24),p.partition_id) AS partition_id,
 CONVERT(varchar(24),p.hobt_id) AS hobt_id,
 a.type AS allocation_type,CONVERT(varchar(24),a.container_id) AS allocation_container_id,
 CONVERT(varchar(33),SYSUTCDATETIME(),126)+N'Z' AS observed_at
FROM sys.dm_os_buffer_descriptors b
JOIN sys.allocation_units a ON a.allocation_unit_id=b.allocation_unit_id AND a.type=1
JOIN sys.partitions p ON p.hobt_id=a.container_id
JOIN sys.indexes i ON i.object_id=p.object_id AND i.index_id=p.index_id AND i.type=1
WHERE b.database_id=@P2 AND b.database_id=DB_ID() AND p.object_id=@P3 AND p.index_id=@P4
 AND p.partition_id=@P5 AND p.hobt_id=@P6 AND p.partition_number=1 AND b.page_level>0"#;

pub(super) struct HeaderWitness {
    allocation: String,
    level: i32,
}
impl HeaderWitness {
    pub(super) fn matches(&self, record: &HeaderRecord) -> bool {
        record.header.allocation_unit_id.as_ref() == Some(&self.allocation)
            && record.header.level == Some(self.level)
            && record.catalog.index_type == Some(1)
            && record.catalog.partition_number == Some(1)
    }
}

pub(super) fn tree(locators: &BTreeMap<PageAddress, LocatedPage>) -> Result<&LocatedPage> {
    let mut pages = locators.values().filter(|page| page.table == TREE);
    let first = pages.next().ok_or_else(|| invalid("owned native tree locator missing"))?;
    if pages.any(|page| !first.same_index(page)) {
        return Err(invalid("owned native tree locators disagree on original index/partition generation"));
    }
    Ok(first)
}

pub(super) async fn locate(
    reader: &mut ActivityReader,
    database: u32,
    locators: &BTreeMap<PageAddress, LocatedPage>,
) -> Result<(PageAddress, LocatedPage)> {
    let tree = tree(locators)?;
    let native_id = |value: &str| value.parse::<i64>().map_err(|_| invalid("native branch identity overflow"));
    let parameters = vec![
        Parameter::Integer(LOOKAHEAD as i64),
        Parameter::Integer(i64::from(database)),
        Parameter::Integer(i64::from(tree.object)),
        Parameter::Integer(i64::from(tree.index)),
        Parameter::Integer(native_id(&tree.partition)?),
        Parameter::Integer(native_id(&tree.hobt)?),
    ];
    let rows = reader.io.query(LOCATOR, parameters, LOOKAHEAD).await.map_err(ReadError::into_error)?;
    decode(database, tree, &rows)
}

fn decode(database: u32, tree: &LocatedPage, rows: &[Rec]) -> Result<(PageAddress, LocatedPage)> {
    let [row] = rows else { return Err(invalid("require exactly one native owned branch locator")) };
    if integer(row, "database_id")? != i64::from(database)
        || integer(row, "index_type")? != 1
        || integer(row, "partition_number")? != 1
        || integer(row, "allocation_type")? != 1
    {
        return Err(invalid("native branch locator has another database/index/allocation population"));
    }
    let allocation = native_catalog_id(row, "allocation_unit_id")?;
    let level = positive(row, "page_level")?;
    let located = LocatedPage {
        object: positive(row, "object_id")?,
        index: positive(row, "index_id")?,
        partition: native_catalog_id(row, "partition_id")?,
        hobt: native_catalog_id(row, "hobt_id")?,
        observed_at: canonical_time(&text(row, "observed_at")?)?,
        table: TREE,
        branch: Some(HeaderWitness { allocation, level }),
    };
    if !tree.same_index(&located) || native_catalog_id(row, "allocation_container_id")? != located.hobt {
        return Err(invalid("native branch locator differs from the original locked tree generation"));
    }
    let coordinate =
        |column| u32::try_from(positive(row, column)?).map_err(|_| invalid("native branch coordinate overflow"));
    Ok((PageAddress { database, file: coordinate("file_id")?, page: coordinate("page_id")? }, located))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resources::test_support::replace, value::Val};

    fn locked() -> BTreeMap<PageAddress, LocatedPage> {
        pages(9, &super::super::tests::rows()).unwrap()
    }
    fn row() -> Rec {
        Rec::of(&[
            ("database_id", Val::Int(9)),
            ("file_id", Val::Int(1)),
            ("page_id", Val::Int(150)),
            ("page_level", Val::Int(1)),
            ("allocation_unit_id", Val::Text("82001".into())),
            ("object_id", Val::Int(1001)),
            ("index_id", Val::Int(1)),
            ("partition_number", Val::Int(1)),
            ("index_type", Val::Int(1)),
            ("partition_id", Val::Text("72001".into())),
            ("hobt_id", Val::Text("72001".into())),
            ("allocation_type", Val::Int(1)),
            ("allocation_container_id", Val::Text("72001".into())),
            ("observed_at", Val::Text("2026-11-20T00:00:01Z".into())),
        ])
    }

    #[test]
    fn one_resident_branch_locator_retains_its_original_tree_allocation_and_native_level() {
        let locked = locked();
        let expected = tree(&locked).unwrap();
        let (address, located) = decode(9, expected, &[row()]).unwrap();
        assert_eq!(address, PageAddress { database: 9, file: 1, page: 150 });
        assert!(expected.same_index(&located));
        let witness = located.branch.unwrap();
        assert_eq!(witness.allocation, "82001");
        assert_eq!(witness.level, 1);
        assert_eq!(located.observed_at, "2026-11-20T00:00:01.000000000Z");
    }

    #[test]
    fn missing_or_multiple_branch_locators_never_turn_into_a_guessed_address_or_skipped_positive() {
        let locked = locked();
        let expected = tree(&locked).unwrap();
        for rows in [vec![], vec![row(), row()]] {
            assert!(decode(9, expected, &rows).is_err());
        }
    }

    #[test]
    fn foreign_missing_leaf_nonrowstore_or_changed_native_generation_locators_are_refused() {
        let locked = locked();
        let expected = tree(&locked).unwrap();
        for (field, value) in [
            ("database_id", Val::Int(8)),
            ("file_id", Val::Int(0)),
            ("page_id", Val::Null),
            ("page_level", Val::Int(0)),
            ("page_level", Val::Int(-1)),
            ("page_level", Val::Null),
            ("object_id", Val::Int(1002)),
            ("index_id", Val::Int(2)),
            ("index_type", Val::Int(2)),
            ("partition_number", Val::Int(2)),
            ("partition_id", Val::Text("72003".into())),
            ("hobt_id", Val::Text("72003".into())),
            ("allocation_container_id", Val::Text("72003".into())),
            ("allocation_type", Val::Int(2)),
            ("allocation_unit_id", Val::Null),
            ("allocation_unit_id", Val::Text("0".into())),
            ("allocation_unit_id", Val::Text("9223372036854775808".into())),
            ("observed_at", Val::Text("unknown".into())),
        ] {
            assert!(decode(9, expected, &[replace(&row(), &[(field, value)])]).is_err(), "{field}");
        }
    }

    #[test]
    fn a_branch_locator_cannot_choose_between_inconsistent_original_tree_generations() {
        let mut locked = locked();
        locked.values_mut().find(|page| page.table == TREE).unwrap().hobt = "foreign-original-generation".into();
        assert!(tree(&locked).is_err());
    }

    #[test]
    fn both_header_endpoints_must_retain_the_located_allocation_level_and_rowstore_partition() {
        let locked = locked();
        let (_, located) = decode(9, tree(&locked).unwrap(), &[row()]).unwrap();
        let witness = located.branch.unwrap();
        let header = replace(
            &crate::activity::page_targets::tests::header(0, 150),
            &[
                ("page_type", Val::Int(2)),
                ("page_level", Val::Int(1)),
                ("alloc_unit_id", Val::Int(82001)),
                ("catalog_partition_number", Val::Int(1)),
            ],
        );
        assert!(witness.matches(&HeaderRecord::decode(&header).unwrap()));
        for (field, value) in [
            ("alloc_unit_id", Val::Int(82002)),
            ("page_level", Val::Int(0)),
            ("page_level", Val::Null),
            ("index_type", Val::Int(2)),
            ("catalog_partition_number", Val::Null),
            ("catalog_partition_number", Val::Int(2)),
        ] {
            assert!(!witness.matches(&HeaderRecord::decode(&replace(&header, &[(field, value)])).unwrap()), "{field}");
        }
    }
}
