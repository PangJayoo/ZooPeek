pub mod search;
pub mod shell;

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::{async_runtime::JoinHandle, AppHandle, Emitter, Manager, State};
use zookeeper_client as zk;

const KEYRING_SERVICE: &str = "zoopeek";
const PASSWORD_REQUIRED: &str = "PASSWORD_REQUIRED";
const NO_AUTH: &str = "NO_AUTH";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuthType {
    #[default]
    None,
    Digest,
    SaslDigestMd5,
}

#[derive(Clone)]
struct ActiveConnection {
    servers: String,
    auth_type: AuthType,
    username: String,
    password: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum WatchKind {
    Children,
    Data,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WatchKey {
    conn_id: String,
    path: String,
    kind: WatchKind,
}

/// 连接和 watcher 统一按 conn_id 管理，避免多 tab 之间互相覆盖。
#[derive(Clone)]
struct ConnectionManager {
    clients: Arc<Mutex<HashMap<String, zk::Client>>>,
    watchers: Arc<Mutex<HashMap<WatchKey, JoinHandle<()>>>>,
    session_watchers: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    active_connections: Arc<Mutex<HashMap<String, ActiveConnection>>>,
    search_manager: search::SearchManager,
}

impl ConnectionManager {
    fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            watchers: Arc::new(Mutex::new(HashMap::new())),
            session_watchers: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(HashMap::new())),
            search_manager: search::SearchManager::new(),
        }
    }

    fn client(&self, conn_id: &str) -> Result<zk::Client, String> {
        self.clients
            .lock()
            .map_err(|error| error.to_string())?
            .get(conn_id)
            .cloned()
            .ok_or_else(|| format!("连接 {conn_id} 不存在或已断开"))
    }

    fn cancel_node_watchers(&self, conn_id: &str) -> Result<(), String> {
        let mut watchers = self.watchers.lock().map_err(|error| error.to_string())?;
        let keys = watchers
            .keys()
            .filter(|key| key.conn_id == conn_id)
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(task) = watchers.remove(&key) {
                // abort 会 drop 任务内持有的一次性 watcher，从服务端注销 watch。
                task.abort();
            }
        }
        Ok(())
    }

    fn cancel_tasks(&self, conn_id: &str) -> Result<(), String> {
        self.cancel_node_watchers(conn_id)?;

        if let Some(task) = self
            .session_watchers
            .lock()
            .map_err(|error| error.to_string())?
            .remove(conn_id)
        {
            task.abort();
        }
        Ok(())
    }

    fn active_connection(&self, conn_id: &str) -> Result<Option<ActiveConnection>, String> {
        self.active_connections
            .lock()
            .map_err(|error| error.to_string())
            .map(|connections| connections.get(conn_id).cloned())
    }

    fn replace_watch(&self, key: WatchKey, task: JoinHandle<()>) -> Result<(), String> {
        if let Some(old_task) = self
            .watchers
            .lock()
            .map_err(|error| error.to_string())?
            .insert(key, task)
        {
            old_task.abort();
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct ConnectResult {
    session_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectRequest {
    conn_id: String,
    servers: String,
    auth_type: AuthType,
    username: String,
    save_password: bool,
    password: Option<String>,
}

#[derive(Serialize)]
struct SaveConnectionResult {
    password_saved: bool,
    keyring_available: bool,
}

#[derive(Serialize)]
struct KeyringStatus {
    available: bool,
}

#[derive(Serialize)]
struct NodeResult {
    data: String,
    data_length: i32,
    version: i32,
    cversion: i32,
    num_children: i32,
    is_ephemeral: bool,
    /// 非 UTF-8 数据标记：前端据此禁止编辑，防止 from_utf8_lossy 后保存导致数据损坏。
    is_binary: bool,
}

#[derive(Deserialize, Serialize)]
struct AclEntry {
    scheme: String,
    id: String,
    perms: i32,
}

fn permission_from_bits(bits: i32) -> Result<zk::Permission, String> {
    if !(0..=31).contains(&bits) {
        return Err(format!("无效的 ACL 权限值：{bits}"));
    }

    let mut permission = zk::Permission::NONE;
    for (bit, value) in [
        (1, zk::Permission::READ),
        (2, zk::Permission::WRITE),
        (4, zk::Permission::CREATE),
        (8, zk::Permission::DELETE),
        (16, zk::Permission::ADMIN),
    ] {
        if bits & bit != 0 {
            permission = permission | value;
        }
    }
    Ok(permission)
}

fn permission_bits(permission: zk::Permission) -> i32 {
    [
        (1, zk::Permission::READ),
        (2, zk::Permission::WRITE),
        (4, zk::Permission::CREATE),
        (8, zk::Permission::DELETE),
        (16, zk::Permission::ADMIN),
    ]
    .into_iter()
    .filter_map(|(bit, value)| permission.has(value).then_some(bit))
    .sum()
}

impl NodeResult {
    fn from_data_and_stat(data: Vec<u8>, stat: zk::Stat) -> Self {
        Self {
            is_binary: std::str::from_utf8(&data).is_err(),
            data: String::from_utf8_lossy(&data).into_owned(),
            data_length: stat.data_length,
            version: stat.version,
            cversion: stat.cversion,
            num_children: stat.num_children,
            is_ephemeral: stat.ephemeral_owner != 0,
        }
    }
}

#[derive(Clone, Serialize)]
struct NodeEvent {
    conn_id: String,
    event_type: String,
    path: String,
    zxid: i64,
}

#[derive(Clone, Serialize)]
struct SessionStateEvent {
    conn_id: String,
    state: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct SavedConnection {
    id: String,
    name: String,
    servers: String,
    #[serde(default)]
    auth_type: AuthType,
    #[serde(default)]
    username: String,
    #[serde(default)]
    save_password: bool,
}

fn zk_error(error: zk::Error) -> String {
    match error {
        zk::Error::NoAuth => format!("{NO_AUTH}:not authorized"),
        _ => error.to_string(),
    }
}

fn connection_error(error: zk::Error) -> String {
    format!("CONNECTION_FAILED:{error}")
}

fn emit_session_state(app: &AppHandle, conn_id: &str, state: &str) {
    let _ = app.emit(
        "zk-session-state",
        SessionStateEvent {
            conn_id: conn_id.to_string(),
            state: state.to_string(),
        },
    );
}

async fn connect_client(spec: &ActiveConnection) -> Result<zk::Client, String> {
    let connector = zk::Client::connector();
    let connector = match spec.auth_type {
        AuthType::None => connector,
        AuthType::Digest => {
            let password = spec.password.as_deref().ok_or(PASSWORD_REQUIRED)?;
            let auth = format!("{}:{password}", spec.username);
            connector.with_auth("digest", auth.as_bytes())
        }
        AuthType::SaslDigestMd5 => {
            let password = spec.password.as_deref().ok_or(PASSWORD_REQUIRED)?;
            connector.with_sasl(zk::SaslOptions::digest_md5(
                spec.username.clone(),
                password.to_string(),
            ))
        }
    };
    connector
        .connect(&spec.servers)
        .await
        .map_err(connection_error)
}

fn install_client(
    app: AppHandle,
    manager: ConnectionManager,
    conn_id: String,
    client: zk::Client,
    spec: ActiveConnection,
) -> Result<String, String> {
    let session_id = client.session_id().to_string();
    let mut state_watcher = client.state_watcher();

    // 新连接成功后再替换旧连接，连接失败时保留原连接。
    manager.cancel_tasks(&conn_id)?;
    manager
        .clients
        .lock()
        .map_err(|error| error.to_string())?
        .insert(conn_id.clone(), client);
    manager
        .active_connections
        .lock()
        .map_err(|error| error.to_string())?
        .insert(conn_id.clone(), spec);
    manager.search_manager.install_connection(&conn_id)?;

    let current_state = state_watcher.state();
    emit_session_state(&app, &conn_id, &format!("{current_state:?}"));

    let app_handle = app.clone();
    let watcher_manager = manager.clone();
    let watcher_conn_id = conn_id.clone();
    let task = tauri::async_runtime::spawn(async move {
        loop {
            let state = state_watcher.changed().await;
            emit_session_state(&app_handle, &watcher_conn_id, &format!("{state:?}"));
            if matches!(
                state,
                zk::SessionState::Expired | zk::SessionState::Closed | zk::SessionState::AuthFailed
            ) {
                // 先移除当前任务的句柄；drop JoinHandle 不会中止正在执行的任务。
                if let Ok(mut watchers) = watcher_manager.session_watchers.lock() {
                    watchers.remove(&watcher_conn_id);
                }
                if let Ok(mut clients) = watcher_manager.clients.lock() {
                    clients.remove(&watcher_conn_id);
                }
                let _ = watcher_manager
                    .search_manager
                    .remove_connection(&watcher_conn_id);

                if state == zk::SessionState::Expired {
                    emit_session_state(&app_handle, &watcher_conn_id, "Connecting");
                    let reconnect_result = watcher_manager
                        .active_connection(&watcher_conn_id)
                        .and_then(|active| active.ok_or_else(|| PASSWORD_REQUIRED.to_string()));
                    match reconnect_result {
                        Ok(active) => match connect_client(&active).await {
                            Ok(new_client) => {
                                if install_client(
                                    app_handle.clone(),
                                    watcher_manager.clone(),
                                    watcher_conn_id.clone(),
                                    new_client,
                                    active,
                                )
                                .is_err()
                                {
                                    emit_session_state(
                                        &app_handle,
                                        &watcher_conn_id,
                                        "ReconnectFailed",
                                    );
                                }
                            }
                            Err(_) => {
                                if let Ok(mut connections) =
                                    watcher_manager.active_connections.lock()
                                {
                                    connections.remove(&watcher_conn_id);
                                }
                                emit_session_state(&app_handle, &watcher_conn_id, "ReconnectFailed")
                            }
                        },
                        Err(_) => {
                            if let Ok(mut connections) = watcher_manager.active_connections.lock() {
                                connections.remove(&watcher_conn_id);
                            }
                            emit_session_state(&app_handle, &watcher_conn_id, "ReconnectFailed")
                        }
                    }
                } else if let Ok(mut connections) = watcher_manager.active_connections.lock() {
                    connections.remove(&watcher_conn_id);
                }
                break;
            }
        }
    });
    if let Some(old_task) = manager
        .session_watchers
        .lock()
        .map_err(|error| error.to_string())?
        .insert(conn_id, task)
    {
        old_task.abort();
    }

    Ok(session_id)
}

fn resolve_password(
    manager: &ConnectionManager,
    conn_id: &str,
    auth_type: &AuthType,
    username: &str,
    save_password: bool,
    password: Option<String>,
) -> Result<Option<String>, String> {
    if auth_type == &AuthType::None {
        return Ok(None);
    }
    if let Some(password) = password.filter(|value| !value.is_empty()) {
        return Ok(Some(password));
    }
    if let Some(active) = manager.active_connection(conn_id)? {
        if active.auth_type == *auth_type && active.username == username {
            if let Some(password) = active.password {
                return Ok(Some(password));
            }
        }
    }
    if save_password {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, conn_id) {
            if let Ok(password) = entry.get_password() {
                return Ok(Some(password));
            }
        }
    }
    Err(PASSWORD_REQUIRED.to_string())
}

#[tauri::command]
async fn connect(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    request: ConnectRequest,
) -> Result<ConnectResult, String> {
    let manager = manager.inner().clone();
    let password = resolve_password(
        &manager,
        &request.conn_id,
        &request.auth_type,
        &request.username,
        request.save_password,
        request.password,
    )?;
    let spec = ActiveConnection {
        servers: request.servers,
        auth_type: request.auth_type,
        username: request.username,
        password,
    };
    let client = connect_client(&spec).await?;
    let session_id = install_client(app, manager, request.conn_id, client, spec)?;

    Ok(ConnectResult { session_id })
}

#[tauri::command]
async fn test_connection(
    servers: String,
    auth_type: AuthType,
    username: String,
    password: Option<String>,
) -> Result<(), String> {
    let spec = ActiveConnection {
        servers,
        auth_type,
        username,
        password,
    };
    let client = connect_client(&spec).await?;
    client.list_children("/").await.map_err(zk_error)?;
    Ok(())
}

#[tauri::command]
fn disconnect(manager: State<'_, ConnectionManager>, conn_id: String) -> Result<(), String> {
    manager.cancel_tasks(&conn_id)?;
    manager.search_manager.remove_connection(&conn_id)?;
    manager
        .clients
        .lock()
        .map_err(|error| error.to_string())?
        .remove(&conn_id);
    manager
        .active_connections
        .lock()
        .map_err(|error| error.to_string())?
        .remove(&conn_id);
    Ok(())
}

#[tauri::command]
async fn list_children(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
) -> Result<Vec<String>, String> {
    let client = manager.client(&conn_id)?;
    let mut children = client.list_children(&path).await.map_err(zk_error)?;
    children.sort();
    Ok(children)
}

#[tauri::command]
fn start_search_index(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    force: bool,
) -> Result<search::SearchIndexTicket, String> {
    let client = manager.client(&conn_id)?;
    search::start_index_build(app, manager.search_manager.clone(), client, conn_id, force)
}

#[tauri::command]
fn get_search_index_status(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
) -> Result<search::SearchIndexStatus, String> {
    manager.search_manager.status(&conn_id)
}

#[tauri::command]
fn cancel_search_index(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    connection_epoch: u64,
    generation: u64,
) -> Result<(), String> {
    manager
        .search_manager
        .cancel_build(&conn_id, connection_epoch, generation)
}

#[tauri::command]
async fn search_nodes(
    manager: State<'_, ConnectionManager>,
    request: search::SearchRequest,
) -> Result<search::SearchResponse, String> {
    search::search_snapshot(manager.search_manager.clone(), request).await
}

#[tauri::command]
async fn get_node(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
) -> Result<NodeResult, String> {
    let client = manager.client(&conn_id)?;
    let (data, stat) = client.get_data(&path).await.map_err(zk_error)?;
    Ok(NodeResult::from_data_and_stat(data, stat))
}

#[tauri::command]
async fn set_data(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
    data: String,
) -> Result<(), String> {
    manager
        .client(&conn_id)?
        .set_data(&path, data.as_bytes(), None)
        .await
        .map_err(zk_error)?;
    Ok(())
}

#[tauri::command]
async fn create_node(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    parent_path: String,
    name: String,
    data: String,
) -> Result<(), String> {
    if name.is_empty() || name.contains('/') {
        return Err("节点名称不能为空且不能包含 /".to_string());
    }
    let path = if parent_path == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent_path.trim_end_matches('/'))
    };
    let client = manager.client(&conn_id)?;
    // ZooKeeper ACL 不会继承；显式复制父节点 ACL，避免新节点意外变成 world:anyone:ALL。
    let (parent_acl, _) = client.get_acl(&parent_path).await.map_err(zk_error)?;
    let options = zk::CreateMode::Persistent.with_acls(zk::Acls::new(&parent_acl));
    client
        .create(&path, data.as_bytes(), &options)
        .await
        .map_err(zk_error)?;
    manager.search_manager.mark_dirty(&conn_id)?;
    Ok(())
}

#[tauri::command]
async fn delete_node(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
) -> Result<(), String> {
    if path == "/" {
        return Err("不能删除根节点".to_string());
    }
    let client = manager.client(&conn_id)?;
    let children = client.list_children(&path).await.map_err(zk_error)?;
    if !children.is_empty() {
        return Err("节点存在子节点，不能直接删除".to_string());
    }
    client.delete(&path, None).await.map_err(zk_error)?;
    manager.search_manager.mark_dirty(&conn_id)?;
    Ok(())
}

pub async fn delete_node_tree(client: &zk::Client, path: &str) -> Result<u32, String> {
    const MAX_NOT_EMPTY_RETRIES: u8 = 8;

    let mut stack = vec![(path.to_string(), false, 0_u8)];
    let mut deleted = 0;
    while let Some((current, children_visited, retry_count)) = stack.pop() {
        if children_visited {
            match client.delete(&current, None).await {
                Ok(()) => deleted += 1,
                Err(zk::Error::NoNode) => {}
                Err(zk::Error::NotEmpty) if retry_count < MAX_NOT_EMPTY_RETRIES => {
                    match client.list_children(&current).await {
                        Ok(children) => {
                            // 遍历后可能并发新增子节点；重新压栈并继续自底向上删除。
                            stack.push((current.clone(), true, retry_count + 1));
                            for child in children {
                                let child_path =
                                    format!("{}/{child}", current.trim_end_matches('/'));
                                stack.push((child_path, false, 0));
                            }
                        }
                        // NotEmpty 后节点可能已被另一个客户端删掉，目标状态已经达成。
                        Err(zk::Error::NoNode) => {}
                        Err(error) => return Err(zk_error(error)),
                    }
                }
                Err(zk::Error::NotEmpty) => {
                    return Err(format!(
                        "递归删除部分完成：已删除 {deleted} 个节点；{current} 持续出现新子节点"
                    ));
                }
                Err(error) => return Err(zk_error(error)),
            }
            continue;
        }

        match client.list_children(&current).await {
            Ok(children) => {
                stack.push((current.clone(), true, retry_count));
                for child in children {
                    let child_path = format!("{}/{child}", current.trim_end_matches('/'));
                    stack.push((child_path, false, 0));
                }
            }
            Err(zk::Error::NoNode) => {}
            Err(error) => return Err(zk_error(error)),
        }
    }
    Ok(deleted)
}

#[tauri::command]
async fn delete_node_recursive(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
) -> Result<u32, String> {
    if path == "/" {
        return Err("不能删除根节点".to_string());
    }
    let client = manager.client(&conn_id)?;
    let deleted = delete_node_tree(&client, &path).await?;
    manager.search_manager.mark_dirty(&conn_id)?;
    Ok(deleted)
}

#[tauri::command]
async fn shell_exec(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    line: String,
) -> Result<String, String> {
    let client = manager.client(&conn_id)?;
    let output = shell::execute(&client, &line).await?;
    // 树结构变更命令执行成功后标记搜索索引过期，与 create_node/delete_node 保持一致。
    let command = line.split_whitespace().next().unwrap_or("");
    if matches!(
        command.to_ascii_lowercase().as_str(),
        "create" | "delete" | "deleteall" | "rmr"
    ) {
        manager.search_manager.mark_dirty(&conn_id)?;
    }
    Ok(output)
}

#[tauri::command]
async fn get_acl(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
) -> Result<Vec<AclEntry>, String> {
    let (acl, _) = manager
        .client(&conn_id)?
        .get_acl(&path)
        .await
        .map_err(zk_error)?;
    Ok(acl
        .into_iter()
        .map(|entry| AclEntry {
            scheme: entry.scheme().to_string(),
            id: entry.id().to_string(),
            perms: permission_bits(entry.permission()),
        })
        .collect())
}

#[tauri::command]
async fn set_acl(
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
    acl: Vec<AclEntry>,
) -> Result<(), String> {
    if acl.is_empty() {
        return Err("ACL 列表不能为空".to_string());
    }
    let acl = acl
        .into_iter()
        .map(|entry| {
            Ok(zk::Acl::new(
                permission_from_bits(entry.perms)?,
                zk::AuthId::new(&entry.scheme, &entry.id),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    manager
        .client(&conn_id)?
        .set_acl(&path, &acl, None)
        .await
        .map_err(zk_error)?;
    Ok(())
}

#[tauri::command]
async fn watch_children(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
) -> Result<Vec<String>, String> {
    let client = manager.client(&conn_id)?;
    let (mut children, _stat, watcher) = client
        .get_and_watch_children(&path)
        .await
        .map_err(zk_error)?;
    children.sort();

    let key = WatchKey {
        conn_id: conn_id.clone(),
        path: path.clone(),
        kind: WatchKind::Children,
    };
    let search_manager = manager.search_manager.clone();
    let search_app = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let event = watcher.changed().await;
        // 已安装的 watcher 命中说明远端结构发生变化，节点搜索快照需要标记为脏。
        // mark_dirty 内部对没有快照的连接是 no-op，不会让 dirty=true 出现于尚未构建完成的连接。
        let _ = search_manager.mark_dirty(&conn_id);
        if let Ok(status) = search_manager.status(&conn_id) {
            let _ = search_app.emit(
                "zk-search-index-state",
                search::SearchIndexStateEvent {
                    conn_id: conn_id.clone(),
                    connection_epoch: status.connection_epoch,
                    generation: status.generation,
                    state: status.state,
                    dirty: status.dirty,
                    stats: status.stats.clone(),
                },
            );
        }
        let _ = app.emit(
            "zk-node-event",
            NodeEvent {
                conn_id,
                event_type: format!("{:?}", event.event_type),
                path: event.path,
                zxid: event.zxid,
            },
        );
    });
    manager.replace_watch(key, task)?;
    Ok(children)
}

#[tauri::command]
async fn watch_data(
    app: AppHandle,
    manager: State<'_, ConnectionManager>,
    conn_id: String,
    path: String,
) -> Result<NodeResult, String> {
    let client = manager.client(&conn_id)?;
    let (data, stat, watcher) = client.get_and_watch_data(&path).await.map_err(zk_error)?;

    let key = WatchKey {
        conn_id: conn_id.clone(),
        path: path.clone(),
        kind: WatchKind::Data,
    };
    let search_manager = manager.search_manager.clone();
    let search_app = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let event = watcher.changed().await;
        let _ = search_manager.mark_dirty(&conn_id);
        if let Ok(status) = search_manager.status(&conn_id) {
            let _ = search_app.emit(
                "zk-search-index-state",
                search::SearchIndexStateEvent {
                    conn_id: conn_id.clone(),
                    connection_epoch: status.connection_epoch,
                    generation: status.generation,
                    state: status.state,
                    dirty: status.dirty,
                    stats: status.stats.clone(),
                },
            );
        }
        let _ = app.emit(
            "zk-node-event",
            NodeEvent {
                conn_id,
                event_type: format!("{:?}", event.event_type),
                path: event.path,
                zxid: event.zxid,
            },
        );
    });
    manager.replace_watch(key, task)?;
    Ok(NodeResult::from_data_and_stat(data, stat))
}

fn connections_file(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|directory| directory.join("connections.json"))
        .map_err(|error| error.to_string())
}

fn read_saved_connections(app: &AppHandle) -> Result<Vec<SavedConnection>, String> {
    let path = connections_file(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&content).map_err(|error| error.to_string())
}

fn write_saved_connections(app: &AppHandle, connections: &[SavedConnection]) -> Result<(), String> {
    let path = connections_file(app)?;
    let parent = path
        .parent()
        .ok_or_else(|| "无法确定配置目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let content = serde_json::to_string_pretty(connections).map_err(|error| error.to_string())?;
    fs::write(path, content).map_err(|error| error.to_string())
}

fn delete_keyring_credential(id: &str) -> Result<(), String> {
    let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, id) else {
        // 没有可用的系统凭证库时不可能存在本应用可访问的条目。
        return Ok(());
    };
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("无法删除系统钥匙串中的密码：{error}")),
    }
}

#[tauri::command]
fn keyring_status() -> KeyringStatus {
    KeyringStatus {
        available: keyring::Entry::store_status().is_ok(),
    }
}

#[tauri::command]
fn save_connection(
    app: AppHandle,
    mut connection: SavedConnection,
    password: Option<String>,
) -> Result<SaveConnectionResult, String> {
    let keyring_available = keyring::Entry::store_status().is_ok();
    let mut password_saved = false;

    if connection.auth_type == AuthType::None {
        connection.username.clear();
        connection.save_password = false;
        delete_keyring_credential(&connection.id)?;
    } else if connection.save_password {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &connection.id) {
            password_saved = match password.as_deref() {
                Some(password) => entry.set_password(password).is_ok(),
                None => entry.get_password().is_ok(),
            };
        }
    } else {
        delete_keyring_credential(&connection.id)?;
    }

    let mut connections = read_saved_connections(&app)?;
    if let Some(existing) = connections.iter_mut().find(|item| item.id == connection.id) {
        *existing = connection;
    } else {
        connections.push(connection);
    }
    write_saved_connections(&app, &connections)?;
    Ok(SaveConnectionResult {
        password_saved,
        keyring_available,
    })
}

#[tauri::command]
fn list_saved_connections(app: AppHandle) -> Result<Vec<SavedConnection>, String> {
    read_saved_connections(&app)
}

#[tauri::command]
fn delete_saved_connection(app: AppHandle, id: String) -> Result<(), String> {
    delete_keyring_credential(&id)?;
    let mut connections = read_saved_connections(&app)?;
    connections.retain(|connection| connection.id != id);
    write_saved_connections(&app, &connections)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(ConnectionManager::new())
        .invoke_handler(tauri::generate_handler![
            connect,
            test_connection,
            disconnect,
            list_children,
            start_search_index,
            get_search_index_status,
            cancel_search_index,
            search_nodes,
            get_node,
            set_data,
            create_node,
            delete_node,
            delete_node_recursive,
            shell_exec,
            get_acl,
            set_acl,
            watch_children,
            watch_data,
            keyring_status,
            save_connection,
            list_saved_connections,
            delete_saved_connection
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
