// Prevents an additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth_rt;
mod commands;
mod error;
mod registry;
mod secret_store;
mod state;
mod sync_policy;
mod sync_rt;

use reqharbor_core::storage::Store;
use state::AppState;
use tauri::{Manager, WindowEvent};

fn main() {
    tauri::Builder::default()
        // One process per data dir: a second launch focuses the first window
        // instead of racing it for the same files (QA-R1-023).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let store = Store::open(&dir)
                .map_err(|e| format!("Cannot open data dir {}: {e}", dir.display()))?;
            let state = AppState::new(store).map_err(|e| e.message)?;
            app.manage(state);
            // Sign-in state: account.json plus the keychain (or 0600 file).
            // The keychain is probed lazily: only with the accounts flag on
            // and a saved account, or on the first sign-in (UX §2: no
            // startup prompt for users who never use accounts).
            let enabled = auth_rt::accounts_enabled();
            let auth = auth_rt::AuthState::load(
                &dir,
                std::sync::Arc::new(secret_store::AppSecrets::for_app(&dir)),
                enabled,
            );
            let signed_in = enabled && auth.account_origin().is_some();
            app.manage(auth);
            // Account mode: the signed-in account and the legacy token.
            auth_rt::attach_store(app.handle());
            let connected = app.state::<AppState>().store().sync_active();
            if signed_in {
                // 06 §9: rediscover the account's server at app start.
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    auth_rt::rediscover_on_start(&handle).await;
                });
            }
            if connected {
                // AC-SYNC-2: pull on startup.
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = sync_rt::run_sync(&handle).await;
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // AC-SYNC-2: pull on focus, at most once per minute.
            if let WindowEvent::Focused(true) = event {
                sync_rt::pull_if_stale(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::get_settings,
            commands::save_settings,
            commands::list_projects,
            commands::get_project_tree,
            commands::create_project,
            commands::rename_project,
            commands::duplicate_project,
            commands::delete_project,
            commands::move_project,
            commands::set_active_environment,
            commands::child_counts,
            commands::create_collection,
            commands::update_collection,
            commands::duplicate_collection,
            commands::delete_collection,
            commands::create_folder,
            commands::rename_folder,
            commands::duplicate_folder,
            commands::delete_folder,
            commands::create_request,
            commands::save_request,
            commands::rename_request,
            commands::duplicate_request,
            commands::delete_request,
            commands::move_node,
            commands::list_environments,
            commands::create_environment,
            commands::update_environment,
            commands::duplicate_environment,
            commands::delete_environment,
            commands::get_variables,
            commands::get_dynamic_variables,
            commands::list_history,
            commands::open_history_entry,
            commands::delete_history_entry,
            commands::clear_history,
            commands::send_request,
            commands::cancel_request,
            commands::get_response_body,
            commands::save_response_body,
            commands::format_body,
            commands::url_to_params,
            commands::params_to_url,
            commands::import_curl,
            commands::looks_like_curl,
            commands::export_curl,
            commands::preview_openapi,
            commands::import_openapi,
            commands::get_sync_status,
            commands::sync_create_workspace,
            commands::sync_connect,
            commands::sync_disconnect,
            commands::sync_rotate_token,
            commands::sync_now,
            commands::sync_workspace_info,
            commands::share_preview,
            commands::share_create,
            commands::share_update,
            commands::share_revoke,
            commands::share_list,
            commands::import_share,
            commands::preview_share,
            commands::server_discover,
            commands::auth_status,
            commands::auth_sign_in_start,
            commands::auth_sign_in_reopen,
            commands::auth_sign_in_cancel,
            commands::auth_sign_out,
            commands::auth_me,
            commands::auth_sessions,
            commands::auth_session_revoke,
            commands::project_locations,
            commands::project_set_location,
            commands::first_sync_candidates,
            commands::first_sync_apply,
            commands::space_notice_resolve,
            commands::space_notice_later,
            commands::legacy_merge,
            commands::legacy_disconnect,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ReqHarbor");
}
