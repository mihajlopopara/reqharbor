// Prevents an additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth_rt;
mod commands;
mod error;
mod graphql_rt;
mod registry;
mod secret_store;
mod state;
mod sync_policy;
mod sync_rt;
mod teams_rt;
mod ws_rt;

use reqharbor_core::storage::Store;
use state::AppState;
use tauri::{Manager, RunEvent, WindowEvent};

fn main() {
    tauri::Builder::default()
        // A reloaded webview has lost every message log: close the sockets
        // it opened instead of leaving them running (and reconnecting) unseen.
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Started
                && let Some(state) = webview.try_state::<AppState>()
            {
                state.ws.close_all();
            }
        })
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
            commands::ws_connect,
            commands::ws_send,
            commands::ws_ping,
            commands::ws_close,
            graphql_rt::graphql_fetch_schema,
            graphql_rt::graphql_cached_schema,
            graphql_rt::graphql_load_schema_file,
            graphql_rt::graphql_store_schema,
            graphql_rt::graphql_clear_schema,
            teams_rt::teams_list,
            teams_rt::team_create,
            teams_rt::team_get,
            teams_rt::team_rename,
            teams_rt::team_delete,
            teams_rt::team_set_role,
            teams_rt::team_remove_member,
            teams_rt::team_leave,
            teams_rt::team_transfer,
            teams_rt::invite_create,
            teams_rt::invite_list,
            teams_rt::invite_revoke,
            teams_rt::invite_preview,
            teams_rt::invite_accept,
            teams_rt::invite_ticket,
            teams_rt::move_preview,
            teams_rt::move_project_to_space,
            teams_rt::move_cancel,
            teams_rt::copy_preview,
            teams_rt::copy_to_project,
            teams_rt::project_duplicate_to,
            teams_rt::role_change_resolve,
            teams_rt::request_edited_by,
        ])
        .build(tauri::generate_context!())
        .expect("error while building ReqHarbor")
        .run(|app, event| {
            // AC-WS-3: every open socket says goodbye (1001) on exit; give
            // the sessions a moment to send it and write their history.
            if let RunEvent::Exit = event
                && let Some(state) = app.try_state::<AppState>()
                && state.ws.close_all() > 0
            {
                let until = std::time::Instant::now() + std::time::Duration::from_millis(800);
                while state.ws.len() > 0 && std::time::Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        });
}
