use crate::{
    api::*,
    dataset::ObjectDescriptor,
    errors::{AppError, AppResult},
    export,
    policy::ResourcePolicy,
    query::Query,
    registry, sample,
    sessions::AppState,
};
use std::{path::Path, sync::Arc};
use tauri::State;

type Backend<'a> = State<'a, Arc<AppState>>;

#[tauri::command]
pub async fn probe_file(path: String) -> AppResult<ProbeResult> {
    if path.len() > 32_768 {
        return Err(AppError::invalid("File path is too long"));
    }
    tokio::task::spawn_blocking(move || registry::probe(Path::new(&path)))
        .await
        .map_err(|_| AppError::internal())?
}
#[tauri::command]
pub async fn open_dataset(
    state: Backend<'_>,
    path: String,
    open_options: OpenOptions,
) -> AppResult<DatasetDescriptor> {
    state.inner().open(path, open_options).await
}
#[tauri::command]
pub async fn get_metadata(
    state: Backend<'_>,
    session_id: String,
    generation: u64,
    object_id: String,
) -> AppResult<DatasetDescriptor> {
    state
        .inner()
        .metadata(session_id, generation, object_id)
        .await
}
#[tauri::command]
pub async fn list_objects(
    state: Backend<'_>,
    session_id: String,
    generation: u64,
    object_id: String,
) -> AppResult<Vec<ObjectDescriptor>> {
    Ok(vec![
        state
            .inner()
            .metadata(session_id, generation, object_id)
            .await?
            .object,
    ])
}
#[tauri::command]
pub async fn select_object(
    state: Backend<'_>,
    session_id: String,
    generation: u64,
    object_id: String,
) -> AppResult<ObjectDescriptor> {
    Ok(state
        .inner()
        .metadata(session_id, generation, object_id)
        .await?
        .object)
}
#[tauri::command]
pub fn begin_operation(
    state: Backend<'_>,
    session_id: String,
    generation: u64,
    object_id: String,
    kind: OperationKind,
) -> AppResult<OperationHandle> {
    state.begin(session_id, generation, object_id, kind)
}
#[tauri::command]
pub fn cancel_operation(state: Backend<'_>, operation_id: String) -> AppResult<CancelResult> {
    state.cancel_operation(&operation_id)
}
#[tauri::command]
pub async fn fetch_page(
    state: Backend<'_>,
    request: OperationRequest<Query>,
) -> AppResult<PageResult> {
    state
        .inner()
        .run(request, OperationKind::Page, |a, g, q, p, _| {
            a.fetch_page(g, &q, p)
        })
        .await
}
#[tauri::command]
pub async fn count_rows(
    state: Backend<'_>,
    request: OperationRequest<Query>,
) -> AppResult<CountResult> {
    state
        .inner()
        .run(request, OperationKind::Count, |a, g, q, p, _| {
            a.count(g, &q, p)
        })
        .await
}
#[tauri::command]
pub async fn column_profile(
    state: Backend<'_>,
    request: OperationRequest<ProfileQuery>,
) -> AppResult<ColumnProfile> {
    state
        .inner()
        .run(request, OperationKind::Profile, |a, g, q, p, _| {
            a.profile(g, &q, p)
        })
        .await
}
#[tauri::command]
pub async fn read_row(
    state: Backend<'_>,
    request: OperationRequest<RowQuery>,
) -> AppResult<RowResult> {
    state
        .inner()
        .run(request, OperationKind::ReadRow, |a, g, q, p, _| {
            a.read_row(g, &q, p)
        })
        .await
}
#[tauri::command]
pub async fn read_document(
    state: Backend<'_>,
    request: OperationRequest<DocumentQuery>,
) -> AppResult<DocumentResult> {
    state
        .inner()
        .run_adapter(request, OperationKind::Document, |a, g, q, p, _| {
            a.document()?.read(g, &q, p)
        })
        .await
}
#[tauri::command]
pub async fn export_data(
    state: Backend<'_>,
    request: OperationRequest<ExportOptions>,
) -> AppResult<ExportResult> {
    state
        .inner()
        .run(request, OperationKind::Export, |a, g, q, p, c| {
            a.export(g, q, p, c)
        })
        .await
}
#[tauri::command]
pub async fn close_dataset(state: Backend<'_>, session_id: String) -> AppResult<CloseResult> {
    state.inner().close(session_id).await
}
#[tauri::command]
pub async fn refresh_dataset(
    state: Backend<'_>,
    session_id: String,
) -> AppResult<DatasetDescriptor> {
    state.inner().refresh(session_id).await
}
#[tauri::command]
pub fn get_resource_policy(state: Backend<'_>) -> ResourcePolicy {
    state.policy.clone()
}
#[tauri::command]
pub async fn generate_sample(
    state: Backend<'_>,
    destination: String,
) -> AppResult<DatasetDescriptor> {
    // create_new/no-clobber publication also protects existing source aliases.
    let permit = state.admit().await?;
    let policy = state.policy.clone();
    let path = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let stage = export::ExportStage::new(&destination, ExportFormat::Parquet, None, &policy)?;
        sample::write_sample(
            stage
                .file
                .to_str()
                .ok_or_else(|| AppError::invalid("Invalid sample path"))?,
        )
        .map_err(AppError::io)?;
        stage.publish()?;
        Ok::<_, AppError>(stage.destination.to_string_lossy().into_owned())
    })
    .await
    .map_err(|_| AppError::internal())??;
    state.inner().open(path, OpenOptions::default()).await
}

#[tauri::command]
pub async fn preview_delimited(
    state: Backend<'_>,
    path: String,
    options: crate::adapters::delimited::DelimitedOptions,
) -> AppResult<crate::adapters::delimited::DelimitedPreview> {
    state.inner().preview_delimited(path, options).await
}

#[tauri::command]
pub async fn inspect_workbook(state: Backend<'_>, path: String) -> AppResult<WorkbookInfo> {
    state.inner().inspect_workbook(path).await
}

#[tauri::command]
pub async fn inspect_database(state: Backend<'_>, path: String) -> AppResult<DatabaseInfo> {
    state.inner().inspect_database(path).await
}

#[tauri::command]
pub fn discard_preview(state: Backend<'_>, token: String) -> AppResult<()> {
    state.discard_preview(&token)
}

#[tauri::command]
pub async fn inspect_hdf5(state: Backend<'_>, path: String, group: String) -> AppResult<Hdf5Info> {
    state.inner().inspect_hdf5(path, group).await
}
