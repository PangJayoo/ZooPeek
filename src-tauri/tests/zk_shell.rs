//! Shell 命令真实链路测试：对本地 ZK 执行 zkCli 风格命令并断言输出。
//! 依赖本地 docker ZK：docker run -d --name zoopeek-zk -p 2181:2181 zookeeper:3.9
//! 运行：cargo test --test zk_shell -- --nocapture

use zookeeper_client as zk;
use zoopeek_lib::shell;

const CLUSTER: &str = "127.0.0.1:2181";
const ROOT: &str = "/zoopeek-shell-test";

async fn run(client: &zk::Client, line: &str) -> String {
    let output = shell::execute(client, line)
        .await
        .unwrap_or_else(|error| panic!("命令执行失败 `{line}`: {error}"));
    println!("zk> {line}\n{output}\n");
    output
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_command_round_trip() {
    let client = zk::Client::connect(CLUSTER).await.expect("connect failed");
    // 幂等清理
    let _ = shell::execute(&client, &format!("deleteall {ROOT}")).await;

    // create 输出 zkCli 风格的 Created 行
    let output = run(&client, &format!("create {ROOT} hello")).await;
    assert_eq!(output, format!("Created {ROOT}"));

    // create 带子路径和含空格数据
    let output = run(&client, &format!("create {ROOT}/child hello world")).await;
    assert_eq!(output, format!("Created {ROOT}/child"));

    // create -s 顺序节点回显完整路径
    let output = run(&client, &format!("create -s {ROOT}/seq-")).await;
    assert!(output.starts_with(&format!("Created {ROOT}/seq-0000000")));

    // ls 输出 [a, b] 格式
    let output = run(&client, &format!("ls {ROOT}")).await;
    assert!(output.contains("child"), "ls 输出应包含 child: {output}");
    assert!(output.starts_with('[') && output.ends_with(']'));

    // get 输出数据 + stat 块
    let output = run(&client, &format!("get {ROOT}/child")).await;
    assert!(output.contains("hello world"));
    assert!(output.contains("dataVersion = 0"));
    assert!(output.contains("numChildren = 0"));

    // set 更新数据并回显 stat
    let output = run(&client, &format!("set {ROOT}/child v2")).await;
    assert!(output.contains("dataVersion = 1"));
    let (data, _) = client.get_data(&format!("{ROOT}/child")).await.unwrap();
    assert_eq!(data, b"v2");

    // set -v 版本校验：错误版本应报 BadVersion 语义
    let error = shell::execute(&client, &format!("set -v 99 {ROOT}/child x"))
        .await
        .expect_err("错误版本应失败");
    println!("set -v 99 -> {error}");
    assert!(error.contains("version"), "错误信息应说明版本无效: {error}");

    // stat 独立查看（此时有 child + seq-* 共 2 个子节点）
    let output = run(&client, &format!("stat {ROOT}")).await;
    assert!(output.contains("cZxid = 0x"));
    assert!(output.contains("numChildren = 2"));

    // getAcl 默认 world:anyone:cdrwa
    let output = run(&client, &format!("getAcl {ROOT}")).await;
    assert!(output.contains("'world,'anyone"), "getAcl 输出: {output}");
    assert!(output.contains(": cdrwa"), "getAcl 输出: {output}");

    // setAcl 只读 + 回读确认（在独立子节点上实验，避免锁死 ROOT 后无法恢复/清理）
    run(&client, &format!("create {ROOT}/acl-lab")).await;
    run(&client, &format!("setAcl {ROOT}/acl-lab world:anyone:r")).await;
    let output = run(&client, &format!("getAcl {ROOT}/acl-lab")).await;
    assert!(output.contains(": r\n") || output.trim_end().ends_with(": r"));
    // 不设回 cdrwa：删除节点只要求父节点有 DELETE 权限，deleteall 收尾即可

    // whoami：无显式认证时 ZK 的 IP provider 也会把客户端 IP 作为身份返回
    let output = run(&client, "whoami").await;
    assert!(
        output.contains(':'),
        "whoami 应输出 scheme:user 格式，实际: {output}"
    );

    // delete 非空节点应报错，deleteall 递归删除
    let error = shell::execute(&client, &format!("delete {ROOT}"))
        .await
        .expect_err("非空节点 delete 应失败");
    println!("delete {ROOT} -> {error}");
    assert!(error.contains("not empty"), "错误信息: {error}");
    let output = run(&client, &format!("deleteall {ROOT}")).await;
    assert!(output.contains("已删除"), "deleteall 输出: {output}");
    assert_eq!(
        client.check_stat(ROOT).await.expect("check_stat failed"),
        None
    );

    // 错误路径：不存在节点 / 未知命令 / 参数不足
    let error = shell::execute(&client, "get /zoopeek-shell-missing")
        .await
        .expect_err("get 不存在节点应失败");
    println!("get missing -> {error}");
    assert!(error.contains("Node does not exist"), "错误信息: {error}");

    let error = shell::execute(&client, "foobar /")
        .await
        .expect_err("未知命令应失败");
    println!("foobar -> {error}");
    assert!(error.contains("未知命令"), "错误信息: {error}");

    let error = shell::execute(&client, "ls")
        .await
        .expect_err("参数不足应失败");
    println!("ls -> {error}");
    assert!(error.contains("用法"), "错误信息: {error}");

    // help 始终可用
    let output = run(&client, "help").await;
    assert!(output.contains("deleteall"));

    println!("SHELL TEST PASSED");
}
