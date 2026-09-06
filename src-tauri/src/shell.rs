//! zkCli 风格命令 shell：解析命令行并对已建立的 ZK 会话执行，输出尽量对齐 zkCli 的文本格式。
//! 历史记录 / 清屏 / 断开等交互命令由前端处理，本模块只负责无状态的命令执行。

use zookeeper_client as zk;

pub const HELP: &str = r#"支持的 zkCli 命令：
  ls <path>                          列出子节点
  stat <path>                        查看节点状态
  get <path>                         查看节点数据和状态
  set [-v <version>] <path> <data>   设置节点数据
  create [-s] [-e] <path> [data] [acl]  创建节点（-s 顺序，-e 临时）
  delete [-v <version>] <path>       删除节点（有子节点时报错）
  deleteall <path> / rmr <path>      递归删除节点及全部子节点
  getAcl <path>                      查看节点 ACL
  setAcl <path> <scheme:id:perms>[,..]  整体替换节点 ACL，如 world:anyone:rwcda
  addauth <scheme> <auth>            追加认证信息，如 addauth digest user:pass
  whoami                             查看当前会话认证身份
  help                               显示本帮助
  history / clear / quit             由界面本地处理
说明：数据包含空格时直接连写即可，无需引号；acl 中 perms 为 rwcda 字母组合或 0-31 整数。"#;

#[derive(Debug, Default, PartialEq)]
struct CreateArgs {
    sequential: bool,
    ephemeral: bool,
    path: String,
    data: String,
    acl: Option<Vec<zk::Acl>>,
}

fn tokenize(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// 权限字母（rwcda）或 0-31 整数，与 zkCli 的 ZKUtil.parseACLs 行为一致。
fn parse_perms(text: &str) -> Result<zk::Permission, String> {
    if text.chars().all(|c| c.is_ascii_digit()) {
        let bits: i32 = text
            .parse()
            .map_err(|_| format!("无效的 ACL 权限值：{text}"))?;
        if !(0..=31).contains(&bits) {
            return Err(format!("无效的 ACL 权限值：{text}"));
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
        return Ok(permission);
    }
    let mut permission = zk::Permission::NONE;
    for ch in text.chars() {
        permission = permission
            | match ch {
                'r' => zk::Permission::READ,
                'w' => zk::Permission::WRITE,
                'c' => zk::Permission::CREATE,
                'd' => zk::Permission::DELETE,
                'a' => zk::Permission::ADMIN,
                _ => return Err(format!("无效的 ACL 权限字符：{ch}（仅支持 rwcda）")),
            };
    }
    Ok(permission)
}

/// zkCli 权限输出顺序：cdrwa。
fn format_perms(permission: zk::Permission) -> String {
    let mut out = String::new();
    for (ch, value) in [
        ('c', zk::Permission::CREATE),
        ('d', zk::Permission::DELETE),
        ('r', zk::Permission::READ),
        ('w', zk::Permission::WRITE),
        ('a', zk::Permission::ADMIN),
    ] {
        if permission.has(value) {
            out.push(ch);
        }
    }
    out
}

/// 解析 zkCli ACL 串：scheme:id:perms[,scheme:id:perms...]。
/// digest 的 id 形如 user:hash，自身带冒号；auth scheme 没有 id。
fn parse_acls(text: &str) -> Result<Vec<zk::Acl>, String> {
    let mut acls = Vec::new();
    for entry in text.split(',') {
        let parts: Vec<&str> = entry.split(':').collect();
        let (id, perms_index) = match parts[0] {
            "digest" if parts.len() >= 4 => (format!("{}:{}", parts[1], parts[2]), 3),
            "auth" if parts.len() >= 2 => (String::new(), 2),
            _ if parts.len() >= 3 => (parts[1].to_string(), 2),
            _ => return Err(format!("无效的 ACL 条目：{entry}")),
        };
        let perms = parts
            .get(perms_index)
            .ok_or_else(|| format!("无效的 ACL 条目：{entry}"))?;
        let permission = parse_perms(perms)?;
        let auth_id = if parts[0] == "auth" {
            zk::AuthId::authed()
        } else {
            zk::AuthId::new(parts[0], &id)
        };
        acls.push(zk::Acl::new(permission, auth_id));
    }
    if acls.is_empty() {
        return Err("ACL 列表不能为空".to_string());
    }
    Ok(acls)
}

fn format_acls(acls: &[zk::Acl]) -> String {
    let mut out = String::new();
    for acl in acls {
        out.push_str(&format!("'{},'{}\n", acl.scheme(), acl.id()));
        out.push_str(&format!(": {}\n", format_perms(acl.permission())));
    }
    out.trim_end_matches('\n').to_string()
}

fn format_time(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%a %b %d %H:%M:%S %Z %Y")
                .to_string()
        })
        .unwrap_or_else(|| millis.to_string())
}

/// zkCli 的 stat 输出块。
fn format_stat(stat: &zk::Stat) -> String {
    format!(
        "cZxid = {:#x}\n\
         ctime = {}\n\
         mZxid = {:#x}\n\
         mtime = {}\n\
         pZxid = {:#x}\n\
         cversion = {}\n\
         dataVersion = {}\n\
         aclVersion = {}\n\
         ephemeralOwner = {:#x}\n\
         dataLength = {}\n\
         numChildren = {}",
        stat.czxid,
        format_time(stat.ctime),
        stat.mzxid,
        format_time(stat.mtime),
        stat.pzxid,
        stat.cversion,
        stat.version,
        stat.aversion,
        stat.ephemeral_owner,
        stat.data_length,
        stat.num_children,
    )
}

fn zk_cli_error(error: zk::Error, path: &str) -> String {
    match error {
        zk::Error::NoNode => format!("Node does not exist: {path}"),
        zk::Error::NodeExists => format!("Node already exists: {path}"),
        zk::Error::NotEmpty => format!("Node not empty: {path}"),
        zk::Error::BadVersion => format!("version No. is not valid: {path}"),
        zk::Error::NoAuth => format!("NO_AUTH:not authorized（权限不足）: {path}"),
        other => other.to_string(),
    }
}

/// create [-s] [-e] <path> [data] [acl]
/// 数据允许包含空格：最后一个参数若能解析为 ACL 则视为 acl，其余并入 data。
fn parse_create_args(tokens: &[String]) -> Result<CreateArgs, String> {
    let mut args = CreateArgs::default();
    let mut index = 1;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "-s" => args.sequential = true,
            "-e" => args.ephemeral = true,
            _ => break,
        }
        index += 1;
    }
    let path = tokens
        .get(index)
        .filter(|token| !token.starts_with('-'))
        .ok_or_else(|| "用法：create [-s] [-e] <path> [data] [acl]".to_string())?;
    args.path = path.clone();
    index += 1;

    let rest = &tokens[index.min(tokens.len())..];
    if rest.is_empty() {
        return Ok(args);
    }
    if rest.len() >= 2 {
        if let Ok(acl) = parse_acls(&rest[rest.len() - 1]) {
            args.acl = Some(acl);
            args.data = rest[..rest.len() - 1].join(" ");
            return Ok(args);
        }
    }
    args.data = rest.join(" ");
    Ok(args)
}

/// set [-v <version>] <path> <data...>，data 允许多段拼接。
fn parse_set_args(tokens: &[String]) -> Result<(String, String, Option<i32>), String> {
    let usage = "用法：set [-v <version>] <path> <data>".to_string();
    let mut index = 1;
    let mut version = None;
    if tokens.get(1).map(String::as_str) == Some("-v") {
        version = Some(
            tokens
                .get(2)
                .ok_or_else(|| usage.clone())?
                .parse::<i32>()
                .map_err(|_| usage.clone())?,
        );
        index = 3;
    }
    let path = tokens.get(index).ok_or_else(|| usage.clone())?.clone();
    let data = tokens[(index + 1).min(tokens.len())..].join(" ");
    if data.is_empty() && tokens.len() <= index + 1 {
        return Err(usage);
    }
    Ok((path, data, version))
}

/// delete [-v <version>] <path>
fn parse_delete_args(tokens: &[String]) -> Result<(String, Option<i32>), String> {
    let usage = "用法：delete [-v <version>] <path>".to_string();
    if tokens.get(1).map(String::as_str) == Some("-v") {
        let version = tokens
            .get(2)
            .ok_or_else(|| usage.clone())?
            .parse::<i32>()
            .map_err(|_| usage.clone())?;
        let path = tokens.get(3).ok_or_else(|| usage.clone())?.clone();
        if tokens.len() > 4 {
            return Err(usage);
        }
        Ok((path, Some(version)))
    } else {
        let path = tokens.get(1).ok_or_else(|| usage.clone())?.clone();
        if tokens.len() > 2 {
            return Err(usage);
        }
        Ok((path, None))
    }
}

/// 执行一条 zkCli 命令，返回要打印的输出文本（可能为空串）。
/// 树结构发生变化的命令（create/delete/deleteall/rmr）由调用方负责标记搜索索引过期。
pub async fn execute(client: &zk::Client, line: &str) -> Result<String, String> {
    let tokens = tokenize(line);
    let Some(command) = tokens.first() else {
        return Ok(String::new());
    };
    match command.to_ascii_lowercase().as_str() {
        "help" => Ok(HELP.to_string()),
        "ls" => {
            let path = tokens
                .get(1)
                .ok_or_else(|| "用法：ls <path>".to_string())?;
            let mut children = client
                .list_children(path)
                .await
                .map_err(|error| zk_cli_error(error, path))?;
            children.sort();
            Ok(format!("[{}]", children.join(", ")))
        }
        "stat" => {
            let path = tokens
                .get(1)
                .ok_or_else(|| "用法：stat <path>".to_string())?;
            match client
                .check_stat(path)
                .await
                .map_err(|error| zk_cli_error(error, path))?
            {
                Some(stat) => Ok(format_stat(&stat)),
                None => Err(format!("Node does not exist: {path}")),
            }
        }
        "get" => {
            let path = tokens
                .get(1)
                .ok_or_else(|| "用法：get <path>".to_string())?;
            let (data, stat) = client
                .get_data(path)
                .await
                .map_err(|error| zk_cli_error(error, path))?;
            Ok(format!(
                "{}\n{}",
                String::from_utf8_lossy(&data),
                format_stat(&stat)
            ))
        }
        "set" => {
            let (path, data, version) = parse_set_args(&tokens)?;
            let stat = client
                .set_data(&path, data.as_bytes(), version)
                .await
                .map_err(|error| zk_cli_error(error, &path))?;
            Ok(format_stat(&stat))
        }
        "create" => {
            let args = parse_create_args(&tokens)?;
            let mode = match (args.sequential, args.ephemeral) {
                (false, false) => zk::CreateMode::Persistent,
                (true, false) => zk::CreateMode::PersistentSequential,
                (false, true) => zk::CreateMode::Ephemeral,
                (true, true) => zk::CreateMode::EphemeralSequential,
            };
            let acls;
            let options = match &args.acl {
                Some(parsed) => {
                    acls = parsed.clone();
                    mode.with_acls(zk::Acls::new(&acls))
                }
                None => mode.with_acls(zk::Acls::anyone_all()),
            };
            let (_stat, sequence) = client
                .create(&args.path, args.data.as_bytes(), &options)
                .await
                .map_err(|error| zk_cli_error(error, &args.path))?;
            if args.sequential {
                Ok(format!("Created {}{}", args.path, sequence))
            } else {
                Ok(format!("Created {}", args.path))
            }
        }
        "delete" => {
            let (path, version) = parse_delete_args(&tokens)?;
            if path == "/" {
                return Err("不能删除根节点".to_string());
            }
            client
                .delete(&path, version)
                .await
                .map_err(|error| zk_cli_error(error, &path))?;
            Ok(String::new())
        }
        "deleteall" | "rmr" => {
            let path = tokens
                .get(1)
                .ok_or_else(|| "用法：deleteall <path>".to_string())?;
            if path == "/" {
                return Err("不能删除根节点".to_string());
            }
            let deleted = crate::delete_node_tree(client, path).await?;
            Ok(format!("已删除 {deleted} 个节点"))
        }
        "getacl" => {
            let path = tokens
                .get(1)
                .ok_or_else(|| "用法：getAcl <path>".to_string())?;
            let (acls, _stat) = client
                .get_acl(path)
                .await
                .map_err(|error| zk_cli_error(error, path))?;
            Ok(format_acls(&acls))
        }
        "setacl" => {
            let path = tokens
                .get(1)
                .ok_or_else(|| "用法：setAcl <path> <scheme:id:perms>[,..]".to_string())?;
            let acl_text = tokens
                .get(2)
                .ok_or_else(|| "用法：setAcl <path> <scheme:id:perms>[,..]".to_string())?;
            if tokens.len() > 3 {
                return Err("用法：setAcl <path> <scheme:id:perms>[,..]（多个条目用英文逗号分隔，不要有空格）".to_string());
            }
            let acls = parse_acls(acl_text)?;
            client
                .set_acl(path, &acls, None)
                .await
                .map_err(|error| zk_cli_error(error, path))?;
            Ok(String::new())
        }
        "addauth" => {
            let scheme = tokens
                .get(1)
                .ok_or_else(|| "用法：addauth <scheme> <auth>".to_string())?;
            let auth = tokens
                .get(2)
                .ok_or_else(|| "用法：addauth <scheme> <auth>".to_string())?;
            client
                .auth(scheme, auth.as_bytes())
                .await
                .map_err(|error| error.to_string())?;
            Ok(String::new())
        }
        "whoami" => {
            let users = client
                .list_auth_users()
                .await
                .map_err(|error| error.to_string())?;
            if users.is_empty() {
                Ok("Not authenticated".to_string())
            } else {
                Ok(users
                    .iter()
                    .map(|user| format!("{}:{}", user.scheme(), user.user()))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
        }
        other => Err(format!("未知命令：{other}，输入 help 查看支持的命令")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_perms_letters_and_int() {
        assert_eq!(parse_perms("rwcda").unwrap(), zk::Permission::ALL);
        assert_eq!(parse_perms("r").unwrap(), zk::Permission::READ);
        assert_eq!(parse_perms("31").unwrap(), zk::Permission::ALL);
        assert_eq!(parse_perms("5").unwrap(), zk::Permission::READ | zk::Permission::CREATE);
        assert!(parse_perms("32").is_err());
        assert!(parse_perms("x").is_err());
    }

    #[test]
    fn format_perms_zkcli_order() {
        assert_eq!(format_perms(zk::Permission::ALL), "cdrwa");
        assert_eq!(format_perms(zk::Permission::READ), "r");
        assert_eq!(
            format_perms(zk::Permission::READ | zk::Permission::WRITE),
            "rw"
        );
    }

    #[test]
    fn parse_acls_variants() {
        let acls = parse_acls("world:anyone:rwcda").unwrap();
        assert_eq!(acls.len(), 1);
        assert_eq!(acls[0].scheme(), "world");
        assert_eq!(acls[0].id(), "anyone");
        assert_eq!(acls[0].permission(), zk::Permission::ALL);

        let acls = parse_acls("digest:u:p:r,world:anyone:r").unwrap();
        assert_eq!(acls.len(), 2);
        assert_eq!(acls[0].id(), "u:p");

        let acls = parse_acls("auth::rwcda").unwrap();
        assert_eq!(acls[0].scheme(), "auth");
        assert_eq!(acls[0].id(), "");

        assert!(parse_acls("world:anyone").is_err());
        assert!(parse_acls("world:anyone:99").is_err());
    }

    #[test]
    fn parse_create_args_variants() {
        let tokens = tokenize("create /a");
        let args = parse_create_args(&tokens).unwrap();
        assert_eq!(args, CreateArgs { path: "/a".into(), ..Default::default() });

        let tokens = tokenize("create -s -e /a hello world");
        let args = parse_create_args(&tokens).unwrap();
        assert!(args.sequential && args.ephemeral);
        assert_eq!(args.data, "hello world");
        assert!(args.acl.is_none());

        let tokens = tokenize("create /a hello world world:anyone:r");
        let args = parse_create_args(&tokens).unwrap();
        assert_eq!(args.data, "hello world");
        assert_eq!(args.acl.unwrap().len(), 1);

        // 数据本身恰好能解析为 ACL 时，两个参数才会被当成 acl，单参数一律视为 data
        let tokens = tokenize("create /a world:anyone:r");
        let args = parse_create_args(&tokens).unwrap();
        assert_eq!(args.data, "world:anyone:r");
        assert!(args.acl.is_none());

        let tokens = tokenize("create -x /a");
        assert!(parse_create_args(&tokens).is_err());
    }

    #[test]
    fn parse_set_and_delete_args() {
        let tokens = tokenize("set /a {\"k\":1}");
        let (path, data, version) = parse_set_args(&tokens).unwrap();
        assert_eq!((path.as_str(), data.as_str(), version), ("/a", "{\"k\":1}", None));

        let tokens = tokenize("set -v 3 /a v2");
        let (path, data, version) = parse_set_args(&tokens).unwrap();
        assert_eq!((path.as_str(), data.as_str(), version), ("/a", "v2", Some(3)));

        assert!(parse_set_args(&tokenize("set /a")).is_err());
        assert!(parse_set_args(&tokenize("set -v x /a v")).is_err());

        let (path, version) = parse_delete_args(&tokenize("delete /a")).unwrap();
        assert_eq!((path.as_str(), version), ("/a", None));
        let (path, version) = parse_delete_args(&tokenize("delete -v 2 /a")).unwrap();
        assert_eq!((path.as_str(), version), ("/a", Some(2)));
        assert!(parse_delete_args(&tokenize("delete")).is_err());
    }
}
