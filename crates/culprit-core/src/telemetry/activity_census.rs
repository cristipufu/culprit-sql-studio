//! Original Activity headers and rows retained for informational session census and offline reanalysis.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ActivityCensusRead {
    pub source: CollectionDescription,
    pub observations: Vec<ActivityObservation>,
    pub read_complete: bool,
}

impl ActivityCensusRead {
    pub fn new(
        source: CollectionDescription,
        observations: Vec<ActivityObservation>,
        read_complete: bool,
    ) -> Result<Self> {
        let read = Self { source, observations, read_complete };
        read.frame()?;
        Ok(read)
    }
    pub fn frame(&self) -> Result<ActivityFrameEvidence<'_>> {
        if self.observations.len() > MAX_ACTIVITY_FRAME_OBSERVATIONS {
            return Err(CollectionStop::InvalidPage.error());
        }
        let rows = self
            .observations
            .iter()
            .map(|observation| ActivityFrameRow { observation, reference: None })
            .collect::<Vec<_>>();
        ActivityFrameEvidence::new(&self.source, &rows, self.read_complete)
    }
    pub fn is_valid(&self) -> bool {
        self.frame().is_ok()
    }
}
