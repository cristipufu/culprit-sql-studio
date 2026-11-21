//! The native dialog delegates one selected artifact; the webview never receives general filesystem access.
use crate::{Error, Result, requests};
use culprit_core::IncidentHistoryReport;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

pub(crate) struct HistoryImports(pub(crate) requests::WindowRequests);
impl Default for HistoryImports {
    fn default() -> Self {
        Self(requests::WindowRequests::serialized())
    }
}
#[tauri::command]
pub(crate) fn begin_incident_history(
    window: tauri::WebviewWindow,
    requests: State<'_, HistoryImports>,
    request_id: String,
) -> Result<()> {
    requests.0.begin(window.label(), request_id)
}
#[tauri::command]
pub(crate) fn cancel_incident_history(
    window: tauri::WebviewWindow,
    requests: State<'_, HistoryImports>,
    request_id: String,
) {
    requests.0.cancel(window.label(), &request_id);
}
#[tauri::command]
pub(crate) async fn open_incident_history(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    requests: State<'_, HistoryImports>,
    request_id: String,
) -> Result<Option<IncidentHistoryReport>> {
    requests
        .0
        .run(window.label(), &request_id, async move {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            app.dialog()
                .file()
                .set_title(culprit_core::t!("view.incident_history.import.title"))
                .add_filter(culprit_core::t!("view.incident_history.import.filter"), &["jsonl", "ndjson"])
                .pick_file(move |picked| {
                    let _ = sender.send(picked);
                });
            let picked = receiver
                .await
                .map_err(|_| Error::invalid(culprit_core::t!("view.incident_history.import.unreadable")))?;
            let Some(picked) = picked else { return Ok(None) };
            let path = picked
                .into_path()
                .map_err(|_| Error::invalid(culprit_core::t!("view.incident_history.import.unreadable")))?;
            culprit_engine::incident_history::open_selected_file(path).await.map(Some)
        })
        .await
}
