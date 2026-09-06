import assert from "node:assert/strict";
import test from "node:test";
import type { ConnectionTab } from "./store.ts";
import { runShellCommand, shellHistoryNavigate } from "./store.ts";

function fakeTab(status = "Disconnected"): ConnectionTab {
  return {
    id: "c1",
    name: "c1",
    servers: "127.0.0.1:2181",
    authType: "none",
    username: "",
    savePassword: false,
    status,
    sessionId: "",
    error: "",
    tree: [],
    expandedKeys: [],
    selectedPath: "",
    selectedNode: null,
    dataDraft: "",
    saving: false,
    aclDraft: [],
    newAcl: { scheme: "world", id: "anyone", permissions: [] },
    aclLoading: false,
    aclSaving: false,
    events: [],
    searchIndexStatus: null,
    searchQuery: "",
    searchScopePath: "/",
    searchResults: [],
    searchTotalMatches: 0,
    searchQuerySeq: 0,
    searchLoading: false,
    searchError: "",
    revealSeq: 0,
    bottomPanel: "shell",
    shellEntries: [],
    shellHistory: [],
    shellHistoryIndex: -1,
    shellDraft: "",
    shellRunning: false,
  };
}

test("shell history navigation walks up/down and restores empty draft", () => {
  const tab = fakeTab();
  tab.shellHistory = ["ls /", "get /a", "stat /a"];

  shellHistoryNavigate(tab, -1);
  assert.equal(tab.shellDraft, "stat /a");
  shellHistoryNavigate(tab, -1);
  assert.equal(tab.shellDraft, "get /a");
  shellHistoryNavigate(tab, -1);
  assert.equal(tab.shellDraft, "ls /");
  // 到顶后继续向上保持第一条
  shellHistoryNavigate(tab, -1);
  assert.equal(tab.shellDraft, "ls /");
  // 向下越过最后一条恢复空输入
  shellHistoryNavigate(tab, 1);
  assert.equal(tab.shellDraft, "get /a");
  shellHistoryNavigate(tab, 1);
  assert.equal(tab.shellDraft, "stat /a");
  shellHistoryNavigate(tab, 1);
  assert.equal(tab.shellDraft, "");
  assert.equal(tab.shellHistoryIndex, -1);
  // 空历史不动作
  tab.shellHistory = [];
  tab.shellDraft = "x";
  shellHistoryNavigate(tab, -1);
  assert.equal(tab.shellDraft, "x");
});

test("local commands clear/history do not require a connection", async () => {
  const tab = fakeTab();
  tab.shellDraft = "ls /";
  await runShellCommand(tab);
  // 断开状态下远端命令报红
  const lastEntry = tab.shellEntries[tab.shellEntries.length - 1];
  assert.equal(lastEntry.kind, "err");
  assert.match(lastEntry.text, /连接未就绪/);
  assert.equal(tab.shellDraft, "");

  tab.shellDraft = "history";
  await runShellCommand(tab);
  assert.ok(
    tab.shellEntries.some(
      (entry) => entry.kind === "out" && entry.text === "0 - ls /"
    )
  );

  tab.shellDraft = "clear";
  await runShellCommand(tab);
  assert.equal(tab.shellEntries.length, 0);
});

test("consecutive duplicate commands are recorded once", async () => {
  const tab = fakeTab();
  tab.shellDraft = "clear";
  await runShellCommand(tab);
  tab.shellDraft = "clear";
  await runShellCommand(tab);
  assert.deepEqual(tab.shellHistory, ["clear"]);
});
