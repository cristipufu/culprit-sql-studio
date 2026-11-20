//! Preserve the native build child only for a closed, nonadaptive inner Hash Match operator.
//! The first native input builds; the second probes. This role does not by itself prove complete consumption.
//! https://learn.microsoft.com/sql/relational-databases/showplan-logical-and-physical-operators-reference
use super::{LocalName, RELOP_METADATA, children, collect_relops, native_boolean};
use roxmltree::Node;

pub(super) fn build_input(relop: Node<'_, '_>) -> Option<u32> {
    if relop.attribute("PhysicalOp") != Some("Hash Match")
        || relop.attribute("LogicalOp") != Some("Inner Join")
        || relop.attribute("Parallel").and_then(native_boolean) != Some(false)
        || relop.attribute("IsAdaptive").is_some_and(|value| native_boolean(value) != Some(false))
        || ["AdaptiveThresholdRows", "EstimatedJoinType"].iter().any(|name| relop.attribute(*name).is_some())
        || has_supplied_row_goal(relop)
    {
        return None;
    }
    let mut operators =
        relop.children().filter(Node::is_element).filter(|node| !RELOP_METADATA.contains(&node.tag_name().name()));
    let hash = operators.next()?;
    if operators.next().is_some()
        || !hash.has_tag_name_local("Hash")
        || hash.attribute("BitmapCreator").is_some_and(|value| native_boolean(value) != Some(false))
        || hash.children().filter(Node::is_element).any(|node| {
            !matches!(
                node.tag_name().name(),
                "DefinedValues" | "HashKeysBuild" | "HashKeysProbe" | "ProbeResidual" | "RelOp"
            )
        })
        || key_count(hash, "HashKeysBuild")? != key_count(hash, "HashKeysProbe")?
    {
        return None;
    }
    let mut inputs = Vec::new();
    collect_relops(hash, &mut inputs);
    let [build, probe] = inputs.as_slice() else { return None };
    if build.parent() != Some(hash) || probe.parent() != Some(hash) {
        return None;
    }
    let build = build.attribute("NodeId")?.parse::<u32>().ok()?;
    let probe = probe.attribute("NodeId")?.parse::<u32>().ok()?;
    let parent = relop.attribute("NodeId")?.parse::<u32>().ok()?;
    (build != probe && build != parent && probe != parent).then_some(build)
}

fn key_count(hash: Node<'_, '_>, name: &str) -> Option<usize> {
    let mut lists = children(hash, name);
    let list = lists.next()?;
    if lists.next().is_some() {
        return None;
    }
    let keys = list.children().filter(Node::is_element).collect::<Vec<_>>();
    (!keys.is_empty()
        && keys.iter().all(|key| {
            key.has_tag_name_local("ColumnReference")
                && key.attribute("Column").is_some_and(|name| !name.trim().is_empty())
                && !key.children().any(|node| node.is_element())
        }))
    .then_some(keys.len())
}

/// A malformed supplied goal normalizes to unknown in the general numeric facts. It cannot strengthen this
/// positive proof. Any native goal on these ancestors or inputs withholds the hash-build completeness route.
fn has_supplied_row_goal(relop: Node<'_, '_>) -> bool {
    relop
        .ancestors()
        .chain(relop.descendants())
        .any(|node| node.has_tag_name_local("RelOp") && node.attribute("EstimateRowsWithoutRowGoal").is_some())
}
