//! An approved profile is checked inside an existing held reader; this guard owns no native work.
use culprit_core::{Result, telemetry::*};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub struct SourceProfileGuard {
    admission: SourceProfileAdmission,
    binding: Arc<Mutex<Option<NativeBinding>>>,
    refused: Arc<AtomicBool>,
}
struct NativeBinding {
    context: NativeReadContext,
    connection_generation: u64,
}
impl SourceProfileGuard {
    pub fn new(request: &CollectionRequest, admission: &SourceProfileAdmission) -> Result<Self> {
        if !admission.matches_request(request) {
            return Err(changed());
        }
        Ok(Self {
            admission: admission.clone(),
            binding: Arc::new(Mutex::new(None)),
            refused: Arc::new(AtomicBool::new(false)),
        })
    }
    fn refuse(&self) -> culprit_core::Error {
        self.refused.store(true, Ordering::Release);
        changed()
    }
    /// Authority discovery can precede native binding; revocation is still checked before login and SET.
    pub fn ensure_active(&self) -> Result<()> {
        if self.refused.load(Ordering::Acquire) || !self.admission.approval_is_active() {
            Err(changed())
        } else {
            Ok(())
        }
    }
    /// The existing original native authority must bind the held socket before any population query.
    pub fn bind_native(&self, context: &NativeReadContext, connection_generation: Option<u64>) -> Result<()> {
        self.ensure_active()?;
        let generation = connection_generation.filter(|generation| *generation > 0).ok_or_else(|| self.refuse())?;
        if !self.admission.matches_native_context(context) {
            return Err(self.refuse());
        }
        let mut binding = self.binding.lock().map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
        if binding.is_some() {
            return Err(self.refuse());
        }
        *binding = Some(NativeBinding { context: context.clone(), connection_generation: generation });
        Ok(())
    }
    /// A settled/retired socket cannot be silently replaced after the original authority was bound.
    pub fn check_connection(&self, connection_generation: Option<u64>) -> Result<()> {
        self.ensure_active()?;
        let binding = self.binding.lock().map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
        if let Some(binding) = binding.as_ref()
            && (connection_generation != Some(binding.connection_generation)
                || !self.admission.matches_native_context(&binding.context))
        {
            return Err(self.refuse());
        }
        Ok(())
    }
    /// The source's existing final witness remains mandatory; matching old profile names is insufficient.
    pub fn verify_final(&self, context: &NativeReadContext, connection_generation: Option<u64>) -> Result<()> {
        self.check_connection(connection_generation)?;
        if !self.admission.matches_native_context(context) {
            return Err(self.refuse());
        }
        let binding = self.binding.lock().map_err(|_| CollectionStop::CancellationUnconfirmed.error())?;
        let binding = binding.as_ref().ok_or_else(|| self.refuse())?;
        if binding.context.target != context.target
            || binding.context.partition != context.partition
            || binding.context.database_writable != context.database_writable
            || binding.context.observed_from != context.observed_from
            || ObservationBounds::new(&binding.context.observed_until, &context.observed_until).is_none()
        {
            return Err(self.refuse());
        }
        Ok(())
    }
}
fn changed() -> culprit_core::Error {
    CollectionStop::SourceChanged.error()
}
