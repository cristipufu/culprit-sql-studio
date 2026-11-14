use super::*;
use async_trait::async_trait;

#[async_trait]
impl RecoveryStore for ObjectRecoveryStore {
    fn id(&self) -> &str {
        &self.id
    }

    async fn checkpoint_head(&self) -> Result<Option<String>> {
        Ok(self.root().await?.0.head_id().map(str::to_owned))
    }
    async fn captured_range(
        &self,
        range: &culprit_core::telemetry::CapturedRange,
    ) -> Result<Option<culprit_core::telemetry::CaptureIdentity>> {
        let Some(range_key) = coverage_key(range)? else {
            return Ok(None);
        };
        let key = format!("coverage/{range_key}.json");
        let (root, _) = self.root().await?;
        if let Some(index) = self.index(&key, &root).await? {
            return self.capture_for_index(&index, range, &root).await.map(Some);
        }
        if let Some(head) = &root.head {
            let manifest = self.manifest(head).await?;
            if coverage_key(&manifest.source.captured_range()?)?.as_deref() == Some(range_key.as_str()) {
                return Ok(Some(manifest.capture));
            }
        }
        Ok(None)
    }

    async fn stage(&self, batch: &PayloadObject, path: &Path) -> Result<StagedBatch> {
        validate_payload(batch)?;
        self.transport.put_file(&batch.object, path, batch.bytes).await?;
        // Content length and object metadata alone cannot establish an immutable payload receipt.
        self.verify_payload(batch).await?;
        Ok(StagedBatch {
            store_id: self.id.clone(),
            object: batch.object.clone(),
            sha256: batch.sha256.clone(),
            bytes: batch.bytes,
        })
    }

    async fn commit(
        &self,
        fence: &Fence,
        expected_head: Option<&str>,
        manifest: &RangeManifest,
        staged: &[StagedBatch],
    ) -> Result<CommitReceipt> {
        manifest.validate()?;
        if manifest.contract != self.contract {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let (mut root, etag) = self.root().await?;
        root.require_owner(fence)?;
        let manifest_digest = manifest.digest()?;
        if let Some(existing) = self.committed_index(&manifest.range_id, &root).await? {
            if existing.manifest_digest != manifest_digest {
                return Err(integrity());
            }
            self.manifest(&existing).await?;
            // A root CAS may have succeeded although its HTTP acknowledgement was lost.
            return Ok(self.receipt(&existing));
        }
        if root.head_id() != expected_head {
            return Err(DeliveryProblem::Conflict.error());
        }
        self.verify_staged(manifest, staged).await?;
        super::super::diagnostic_recovery::validate_staged(self, manifest).await?;
        root.diagnostic_heads.observe(manifest, &manifest_digest)?;
        self.repair_indexes(&root).await?;
        for advance in &manifest.advances {
            if root.checkpoints.get(&advance.stream_key) != advance.previous.as_ref() {
                return Err(DeliveryProblem::Conflict.error());
            }
            root.checkpoints.insert(advance.stream_key.clone(), advance.next.clone());
        }
        let sequence = manifest
            .capture
            .revision
            .as_ref()
            .ok_or_else(integrity)?
            .sequence
            .parse::<u64>()
            .map_err(|_| integrity())?;
        let order = root.head.as_ref().map_or(Ok(1), |head| head.order.checked_add(1).ok_or_else(integrity))?;
        root.pending.observe(order, manifest)?;
        let index = CommitIndex {
            store_id: self.id.clone(),
            order,
            range_id: manifest.range_id.clone(),
            manifest_digest,
            high_sequence: root.high_sequence.max(sequence),
            previous: root.head_id().map(str::to_owned),
        };
        self.immutable(&index.manifest_key(), encode(manifest, MAX_MANIFEST_BYTES)?).await?;
        root.indexed_order = order - 1;
        root.high_sequence = root.high_sequence.max(sequence);
        root.head = Some(index.clone());
        // This CAS is the sole commit point, including the stream checkpoint advances.
        self.replace_root(&root, &etag).await?;
        // The root already authorizes recovery even if either index write is interrupted.
        self.repair_indexes(&root).await?;
        Ok(self.receipt(&index))
    }

    async fn load(&self, range_id: &str) -> Result<RangeManifest> {
        if !valid_digest(range_id) {
            return Err(integrity());
        }
        let (root, _) = self.root().await?;
        let index = self.committed_index(range_id, &root).await?.ok_or_else(|| DeliveryProblem::NotReady.error())?;
        self.manifest(&index).await
    }

    async fn capture_manifest(&self, capture: &culprit_core::telemetry::CaptureIdentity) -> Result<RangeManifest> {
        let manifest = self.load(&range_id_for(&self.contract, capture)?).await?;
        if &manifest.capture != capture {
            return Err(integrity());
        }
        Ok(manifest)
    }
    async fn diagnostic_heads(&self, cursor: Option<&str>, limit: u32) -> Result<DiagnosticHeadPage> {
        let (root, _) = self.root().await?;
        root.diagnostic_heads.page(cursor, limit, root.high_sequence)
    }
    async fn pending_ranges(&self, cursor: Option<&str>, limit: u32, order: RecoveryOrder) -> Result<RecoveryPage> {
        if !(1..=128).contains(&limit) {
            return Err(DeliveryProblem::Capacity.error());
        }
        let (root, _) = self.root().await?;
        let (ids, next_cursor) =
            root.pending.page(cursor, 1, &self.id, root.head.as_ref().map_or(0, |head| head.order), order)?;
        let mut ranges = Vec::new();
        for id in ids {
            let index = self.committed_index(&id, &root).await?.ok_or_else(integrity)?;
            ranges.push(self.manifest(&index).await?);
        }
        Ok(RecoveryPage { ranges, next_cursor, high_sequence: root.high_sequence })
    }
    async fn pending_diagnostic_inputs(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        let (root, _) = self.root().await?;
        let (ids, next_cursor) =
            root.pending.diagnostic_page(cursor, limit, &self.id, root.head.as_ref().map_or(0, |head| head.order))?;
        let mut ranges = Vec::new();
        for id in ids {
            let index = self.committed_index(&id, &root).await?.ok_or_else(integrity)?;
            ranges.push(self.manifest(&index).await?);
        }
        Ok(RecoveryPage { ranges, next_cursor, high_sequence: root.high_sequence })
    }
    async fn retire(&self, fence: &Fence, publication: &super::super::sink::PublishedRange) -> Result<()> {
        let range = publication.committed();
        if range.store_id() != self.id {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let (mut root, etag) = self.root().await?;
        root.require_owner(fence)?;
        let index = self.committed_index(&range.manifest().range_id, &root).await?.ok_or_else(integrity)?;
        if index.manifest_digest != range.manifest_digest() {
            return Err(integrity());
        }
        root.pending.retire(&range.manifest().range_id);
        self.replace_root(&root, &etag).await
    }
    async fn recover(&self, cursor: Option<&str>, limit: u32) -> Result<RecoveryPage> {
        if limit == 0 || limit > MAX_RECOVERY_PAGE {
            return Err(DeliveryProblem::Capacity.error());
        }
        let (root, _) = self.root().await?;
        let Some(head) = &root.head else {
            if cursor.is_some() {
                return Err(integrity());
            }
            return Ok(RecoveryPage { ranges: vec![], next_cursor: None, high_sequence: 0 });
        };
        let mut cursor = match cursor {
            Some(value) if value.len() <= MAX_INDEX_BYTES => {
                serde_json::from_str::<RecoveryCursor>(value).map_err(|_| integrity())?
            }
            Some(_) => return Err(DeliveryProblem::Capacity.error()),
            None => RecoveryCursor {
                store_id: self.id.clone(),
                after: 0,
                head: head.clone(),
                high_sequence: root.high_sequence,
            },
        };
        cursor.validate(&root)?;
        let boundary = self
            .index(&cursor.head.order_key(), &root)
            .await?
            .or_else(|| root.head.as_ref().filter(|head| head.order == cursor.head.order).cloned())
            .ok_or_else(integrity)?;
        if boundary != cursor.head {
            return Err(integrity());
        }
        let mut ranges = Vec::new();
        let mut bytes = 0usize;
        while cursor.after < cursor.head.order && ranges.len() < limit as usize {
            let order = cursor.after + 1;
            let index = self
                .index(&format!("commits/{order:020}.json"), &root)
                .await?
                .or_else(|| (order == cursor.head.order).then(|| cursor.head.clone()))
                .ok_or_else(integrity)?;
            if index.order != order || (order == cursor.head.order && index != cursor.head) {
                return Err(integrity());
            }
            let manifest = self.manifest(&index).await?;
            let length = encode(&manifest, MAX_MANIFEST_BYTES)?.len();
            if !ranges.is_empty() && bytes + length > MAX_MANIFEST_BYTES {
                break;
            }
            bytes += length;
            ranges.push(manifest);
            cursor.after = order;
        }
        let next_cursor = if cursor.after < cursor.head.order {
            Some(String::from_utf8(encode(&cursor, MAX_INDEX_BYTES)?).map_err(|_| integrity())?)
        } else {
            None
        };
        Ok(RecoveryPage { ranges, next_cursor, high_sequence: cursor.high_sequence })
    }

    async fn open_object(&self, batch: &PayloadObject) -> Result<RecoveryReader> {
        validate_payload(batch)?;
        let value = self.transport.get(&batch.object).await?.ok_or_else(integrity)?;
        if value.bytes != batch.bytes {
            return Err(integrity());
        }
        // The common RecoveryStore reader checks the complete length and digest at EOF.
        Ok(value.body)
    }
}
impl ObjectRecoveryStore {
    pub(super) async fn capture_for_index(
        &self,
        index: &CommitIndex,
        requested: &culprit_core::telemetry::CapturedRange,
        root: &Root,
    ) -> Result<culprit_core::telemetry::CaptureIdentity> {
        let committed = self.committed_index(&index.range_id, root).await?.ok_or_else(integrity)?;
        if &committed != index {
            return Err(integrity());
        }
        let manifest = self.manifest(index).await?;
        if coverage_key(&manifest.source.captured_range()?)? != coverage_key(requested)? {
            return Err(integrity());
        }
        Ok(manifest.capture)
    }
    async fn committed_index(&self, range_id: &str, root: &Root) -> Result<Option<CommitIndex>> {
        let Some(index) = self.index(&format!("ranges/{range_id}.json"), root).await? else {
            return Ok(root.head.as_ref().filter(|head| head.range_id == range_id).cloned());
        };
        if index.range_id != range_id {
            return Err(integrity());
        }
        Ok(Some(index))
    }
    fn receipt(&self, index: &CommitIndex) -> CommitReceipt {
        CommitReceipt {
            store_id: self.id.clone(),
            manifest_digest: index.manifest_digest.clone(),
            head: index.range_id.clone(),
        }
    }
    async fn verify_staged(&self, manifest: &RangeManifest, staged: &[StagedBatch]) -> Result<()> {
        if staged.len() != manifest.batches.len() {
            return Err(integrity());
        }
        for (batch, receipt) in manifest.batches.iter().zip(staged) {
            if receipt.store_id != self.id
                || receipt.object != batch.object
                || receipt.sha256 != batch.sha256
                || receipt.bytes != batch.bytes
            {
                return Err(integrity());
            }
            self.verify_payload(&PayloadObject::from(batch)).await?;
        }
        Ok(())
    }
}
