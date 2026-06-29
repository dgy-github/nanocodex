<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";

  // Mirrors the Rust `UiEvent` enum (serde tag = "kind", snake_case).
  type UiEvent =
    | { kind: "ready"; model: string; sandbox: string; workspace: string }
    | { kind: "assistant"; text: string }
    | { kind: "tool_start"; name: string; args: string }
    | { kind: "tool_result"; name: string; result: string }
    | { kind: "approval"; id: number; command: string; reason: string; cwd: string; details: string }
    | {
        kind: "done";
        final_text: string;
        iterations: number;
        stop_reason: string;
        tools_used: string[];
        usage: UsageMap;
        context_edit: ContextEditStats;
      }
    | { kind: "error"; message: string };

  type Approval = { id: number; command: string; reason: string; cwd: string; details: string };
  let approval = $state<Approval | null>(null);

  type Settings = {
    model: string;
    base_url: string;
    sandbox_mode: string;
    approval_policy: string;
    reasoning_effort: string;
    max_iterations: number;
    max_tool_calls: number;
    context_edit_enabled: boolean;
    context_edit_max_chars: number;
    context_edit_keep_recent_messages: number;
    context_edit_max_tool_result_chars: number;
    api_key_masked: string;
    has_api_key: boolean;
    available_models: string[];
    sandbox_modes: string[];
    approval_policies: string[];
  };
  type ConfigLocation = {
    config_path: string;
    config_dir: string;
  };
  let settings = $state<Settings | null>(null);
  let configLocation = $state<ConfigLocation | null>(null);
  let apiKeyInput = $state("");
  let saving = $state(false);

  type Checkpoint = {
    id: string;
    label: string;
    created_at: string;
    files: number;
    skipped: number;
    total_bytes: number;
  };
  type RestoreReport = {
    checkpoint_id: string;
    safety_checkpoint_id?: string | null;
    restored_files: number;
    deleted_files: number;
  };
  let checkpointOpen = $state(false);
  let checkpoints = $state<Checkpoint[]>([]);
  let checkpointLabel = $state("");
  let checkpointBusy = $state(false);

  type CustomCommand = {
    scope: string;
    name: string;
    slash: string;
    path: string;
  };
  let commandOpen = $state(false);
  let customCommands = $state<CustomCommand[]>([]);
  let commandArgs = $state<Record<string, string>>({});
  let commandBusy = $state(false);

  type MemoryEntry = {
    ts: number;
    tags: string[];
    text: string;
  };
  type MemorySnapshot = {
    path: string;
    count: number;
    entries: MemoryEntry[];
  };
  type MemoryMergeReport = {
    path: string;
    removed: number;
    count: number;
  };
  let memoryOpen = $state(false);
  let memory = $state<MemorySnapshot | null>(null);
  let memoryNote = $state("");
  let memoryTags = $state("");
  let memoryBusy = $state(false);

  type UsageMap = Record<string, number>;
  type ContextEditStats = {
    original_chars: number;
    edited_chars: number;
    compressed_tool_results: number;
    dropped_messages: number;
  };
  type TurnMetrics = {
    usage: UsageMap;
    iterations: number;
    tool_calls: number;
    tools_used: string[];
    stop_reason: string;
    context_edit: ContextEditStats;
  };
  let usageOpen = $state(false);
  let lastMetrics = $state<TurnMetrics | null>(null);
  let sessionUsage = $state<UsageMap>({});
  let sessionModelCalls = $state(0);
  let sessionToolCalls = $state(0);
  let sessionCompressedToolResults = $state(0);
  let sessionDroppedMessages = $state(0);

  type Msg =
    | { role: "user" | "assistant" | "note"; text: string }
    | { role: "tool"; name: string; args?: string; result?: string };

  let messages = $state<Msg[]>([]);
  let input = $state("");
  let busy = $state(false);
  let header = $state("connecting…");
  let scroller: HTMLDivElement;

  function scrollDown() {
    queueMicrotask(() => scroller?.scrollTo({ top: scroller.scrollHeight }));
  }

  onMount(async () => {
    // Header falls back to a direct status call until the agent thread is Ready.
    try {
      const s = await invoke<{ model: string; sandbox: string }>("get_status");
      header = `${s.model} · ${s.sandbox}`;
      const initialSettings = await invoke<Settings>("get_settings");
      if (!initialSettings.has_api_key) {
        settings = initialSettings;
        configLocation = await invoke<ConfigLocation>("get_config_location");
        apiKeyInput = "";
        header = "needs config";
        messages.push({ role: "note", text: "API key required. Add it in Settings, then Save." });
      }
    } catch (e) {
      header = "config error";
    }

    await listen<UiEvent>("ncx://event", (ev) => {
      const p = ev.payload;
      switch (p.kind) {
        case "ready":
          header = `${p.model} · ${p.sandbox}`;
          break;
        case "assistant":
          messages.push({ role: "assistant", text: p.text });
          break;
        case "tool_start":
          messages.push({ role: "tool", name: p.name, args: p.args });
          break;
        case "approval":
          approval = { id: p.id, command: p.command, reason: p.reason, cwd: p.cwd, details: p.details };
          break;
        case "tool_result": {
          // Attach the result to the most recent unfinished tool entry.
          const last = [...messages].reverse().find(
            (m) => m.role === "tool" && m.name === p.name && m.result === undefined,
          ) as Extract<Msg, { role: "tool" }> | undefined;
          if (last) last.result = p.result;
          else messages.push({ role: "tool", name: p.name, result: p.result });
          break;
        }
        case "done":
          recordMetrics(p);
          // The completed reply already arrived as an `assistant` event; only a
          // non-normal stop adds a note.
          if (p.stop_reason !== "completed") {
            messages.push({ role: "note", text: `[${p.stop_reason}] ${p.final_text}` });
          }
          busy = false;
          break;
        case "error":
          messages.push({ role: "note", text: `Error: ${p.message}` });
          if (p.message.includes("API key")) {
            header = "needs config";
            if (!settings) void openSettings();
          }
          busy = false;
          break;
      }
      scrollDown();
    });
  });

  async function queuePrompt(visibleText: string, promptText: string) {
    if (!visibleText.trim() || busy) return;
    messages.push({ role: "user", text: visibleText });
    busy = true;
    scrollDown();
    try {
      await invoke("send_prompt", { text: promptText });
    } catch (e) {
      messages.push({ role: "note", text: `Failed to send: ${e}` });
      busy = false;
    }
  }

  async function expandTypedCommand(text: string) {
    if (!text.startsWith("/")) return text;
    const match = text.match(/^(\S+)(?:\s+([\s\S]*))?$/);
    if (!match) return text;
    try {
      return await invoke<string>("expand_custom_command", {
        slash: match[1],
        arg: match[2] ?? "",
      });
    } catch {
      return text;
    }
  }

  async function send() {
    const text = input.trim();
    if (!text || busy) return;
    input = "";
    const prompt = await expandTypedCommand(text);
    await queuePrompt(text, prompt);
  }

  function onKey(e: KeyboardEvent) {
    // Enter sends; Shift+Enter inserts a newline.
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  async function decide(approved: boolean) {
    if (!approval) return;
    const id = approval.id;
    approval = null;
    try {
      await invoke("approve", { id, approved });
    } catch (e) {
      messages.push({ role: "note", text: `Approval failed: ${e}` });
    }
  }

  async function loadSettingsPanel() {
    const [loadedSettings, loadedLocation] = await Promise.all([
      invoke<Settings>("get_settings"),
      invoke<ConfigLocation>("get_config_location"),
    ]);
    settings = loadedSettings;
    configLocation = loadedLocation;
    apiKeyInput = "";
  }

  async function openSettings() {
    try {
      await loadSettingsPanel();
    } catch (e) {
      messages.push({ role: "note", text: `Settings load failed: ${e}` });
    }
  }

  async function openConfigFile() {
    try {
      await invoke("open_config_file");
      configLocation = await invoke<ConfigLocation>("get_config_location");
    } catch (e) {
      messages.push({ role: "note", text: `Open config failed: ${e}` });
    }
  }

  async function openConfigDir() {
    try {
      await invoke("open_config_dir");
      configLocation = await invoke<ConfigLocation>("get_config_location");
    } catch (e) {
      messages.push({ role: "note", text: `Open config folder failed: ${e}` });
    }
  }

  async function saveSettings() {
    if (!settings) return;
    saving = true;
    const updates: Record<string, string> = {
      model: settings.model,
      base_url: settings.base_url,
      sandbox_mode: settings.sandbox_mode,
      approval_policy: settings.approval_policy,
      reasoning_effort: settings.reasoning_effort,
      max_iterations: String(settings.max_iterations),
      max_tool_calls: String(settings.max_tool_calls),
      context_edit_enabled: String(settings.context_edit_enabled),
      context_edit_max_chars: String(settings.context_edit_max_chars),
      context_edit_keep_recent_messages: String(settings.context_edit_keep_recent_messages),
      context_edit_max_tool_result_chars: String(settings.context_edit_max_tool_result_chars),
    };
    if (apiKeyInput.trim()) updates.api_key = apiKeyInput.trim();
    try {
      await invoke("save_settings", { updates });
      settings = null;
      apiKeyInput = "";
    } catch (e) {
      messages.push({ role: "note", text: `Save failed: ${e}` });
    }
    saving = false;
  }

  async function loadCheckpoints() {
    checkpoints = await invoke<Checkpoint[]>("get_checkpoints");
  }

  async function openCheckpoints() {
    checkpointOpen = true;
    checkpointBusy = true;
    try {
      await loadCheckpoints();
    } catch (e) {
      messages.push({ role: "note", text: `Checkpoint load failed: ${e}` });
    }
    checkpointBusy = false;
  }

  async function saveCheckpoint() {
    checkpointBusy = true;
    try {
      const cp = await invoke<Checkpoint>("create_checkpoint", { label: checkpointLabel });
      checkpointLabel = "";
      await loadCheckpoints();
      messages.push({ role: "note", text: `Checkpoint saved: ${cp.id}` });
    } catch (e) {
      messages.push({ role: "note", text: `Checkpoint failed: ${e}` });
    }
    checkpointBusy = false;
  }

  async function restoreCheckpoint(id: string) {
    if (busy || checkpointBusy) return;
    if (!window.confirm(`Restore checkpoint ${id}?`)) return;
    checkpointBusy = true;
    try {
      const report = await invoke<RestoreReport>("restore_checkpoint", { id });
      await loadCheckpoints();
      messages.push({
        role: "note",
        text: `Restored ${report.checkpoint_id}: ${report.restored_files} file(s), ${report.deleted_files} removed.`,
      });
    } catch (e) {
      messages.push({ role: "note", text: `Restore failed: ${e}` });
    }
    checkpointBusy = false;
  }

  async function loadCommands() {
    customCommands = await invoke<CustomCommand[]>("get_custom_commands");
  }

  async function openCommands() {
    commandOpen = true;
    commandBusy = true;
    try {
      await loadCommands();
    } catch (e) {
      messages.push({ role: "note", text: `Command load failed: ${e}` });
    }
    commandBusy = false;
  }

  async function runCustomCommand(cmd: CustomCommand) {
    if (busy || commandBusy) return;
    const arg = (commandArgs[cmd.slash] ?? "").trim();
    const visible = arg ? `${cmd.slash} ${arg}` : cmd.slash;
    commandBusy = true;
    try {
      const prompt = await invoke<string>("expand_custom_command", { slash: cmd.slash, arg });
      commandOpen = false;
      await queuePrompt(visible, prompt);
    } catch (e) {
      messages.push({ role: "note", text: `Command failed: ${e}` });
    }
    commandBusy = false;
  }

  async function loadMemory() {
    memory = await invoke<MemorySnapshot>("get_memory");
  }

  async function openMemory() {
    memoryOpen = true;
    memoryBusy = true;
    try {
      await loadMemory();
    } catch (e) {
      messages.push({ role: "note", text: `Memory load failed: ${e}` });
    }
    memoryBusy = false;
  }

  async function saveMemoryNote() {
    const text = memoryNote.trim();
    if (!text || memoryBusy) return;
    memoryBusy = true;
    try {
      const tags = memoryTags
        .split(/[,\s]+/)
        .map((tag) => tag.trim())
        .filter(Boolean);
      memory = await invoke<MemorySnapshot>("remember_note", { text, tags });
      memoryNote = "";
      memoryTags = "";
      messages.push({ role: "note", text: "Memory note saved." });
    } catch (e) {
      messages.push({ role: "note", text: `Memory save failed: ${e}` });
    }
    memoryBusy = false;
  }

  async function openMemoryFile() {
    memoryBusy = true;
    try {
      await invoke("open_memory_file");
      await loadMemory();
    } catch (e) {
      messages.push({ role: "note", text: `Open memory failed: ${e}` });
    }
    memoryBusy = false;
  }

  async function mergeMemory(mode: "heuristic" | "llm") {
    if (memoryBusy) return;
    memoryBusy = true;
    try {
      const report = await invoke<MemoryMergeReport>(
        mode === "llm" ? "summarize_memory" : "consolidate_memory",
      );
      await loadMemory();
      messages.push({
        role: "note",
        text: `Memory merged: ${report.removed} removed, ${report.count} remaining.`,
      });
    } catch (e) {
      messages.push({ role: "note", text: `Memory merge failed: ${e}` });
    }
    memoryBusy = false;
  }

  function usageValue(usage: UsageMap | null | undefined, key: string) {
    return usage?.[key] ?? 0;
  }

  function totalTokens(usage: UsageMap | null | undefined) {
    return usageValue(usage, "prompt_tokens") + usageValue(usage, "completion_tokens");
  }

  function addUsage(left: UsageMap, right: UsageMap | null | undefined) {
    const merged: UsageMap = { ...left };
    for (const [key, value] of Object.entries(right ?? {})) {
      merged[key] = (merged[key] ?? 0) + Number(value || 0);
    }
    return merged;
  }

  function emptyContextEdit(): ContextEditStats {
    return {
      original_chars: 0,
      edited_chars: 0,
      compressed_tool_results: 0,
      dropped_messages: 0,
    };
  }

  function savedContextChars(stats: ContextEditStats | null | undefined) {
    if (!stats) return 0;
    return Math.max(0, stats.original_chars - stats.edited_chars);
  }

  function recordMetrics(turn: Extract<UiEvent, { kind: "done" }>) {
    const tools = turn.tools_used ?? [];
    const contextEdit = turn.context_edit ?? emptyContextEdit();
    lastMetrics = {
      usage: turn.usage ?? {},
      iterations: turn.iterations ?? 0,
      tool_calls: tools.length,
      tools_used: tools,
      stop_reason: turn.stop_reason,
      context_edit: contextEdit,
    };
    sessionUsage = addUsage(sessionUsage, turn.usage);
    sessionModelCalls += turn.iterations ?? 0;
    sessionToolCalls += tools.length;
    sessionCompressedToolResults += contextEdit.compressed_tool_results ?? 0;
    sessionDroppedMessages += contextEdit.dropped_messages ?? 0;
  }

  function resetUsage() {
    lastMetrics = null;
    sessionUsage = {};
    sessionModelCalls = 0;
    sessionToolCalls = 0;
    sessionCompressedToolResults = 0;
    sessionDroppedMessages = 0;
  }

  function formatMemoryTime(ts: number) {
    if (!ts) return "";
    return new Date(ts * 1000).toLocaleString();
  }
</script>

<main>
  <header>
    <span class="brand">nanocodex</span>
    <span class="meta">{header}</span>
    {#if busy}<span class="spinner" title="working…">●</span>{/if}
    <button class="gear" title="Settings" onclick={openSettings} aria-label="Settings">⚙</button>
    <button class="toolbtn" title="Usage" onclick={() => (usageOpen = true)} aria-label="Usage">U</button>
    <button class="toolbtn" title="Memory" onclick={openMemory} aria-label="Memory">M</button>
    <button class="toolbtn" title="Custom commands" onclick={openCommands} aria-label="Custom commands">/</button>
    <button class="toolbtn" title="Checkpoints" onclick={openCheckpoints} aria-label="Checkpoints">CP</button>
  </header>

  <div class="scroll" bind:this={scroller}>
    {#if messages.length === 0}
      <p class="empty">Ask me to inspect or edit the workspace. Try “list the files” or
        “create hello.txt with apply_patch”.</p>
    {/if}
    {#each messages as m}
      {#if m.role === "user"}
        <div class="msg user"><div class="bubble">{m.text}</div></div>
      {:else if m.role === "assistant"}
        <div class="msg assistant"><div class="bubble">{m.text}</div></div>
      {:else if m.role === "note"}
        <div class="msg note">{m.text}</div>
      {:else if m.role === "tool"}
        <div class="tool">
          <span class="tname">⚙ {m.name}</span>
          {#if m.args}<code class="targs">{m.args}</code>{/if}
          {#if m.result !== undefined}
            <pre class="tresult">{m.result}</pre>
          {:else}
            <span class="trunning">running…</span>
          {/if}
        </div>
      {/if}
    {/each}
  </div>

  <footer>
    <textarea
      bind:value={input}
      onkeydown={onKey}
      placeholder="Message nanocodex…  (Enter to send, Shift+Enter for newline)"
      rows="2"
    ></textarea>
    <button onclick={send} disabled={busy || input.trim() === ""}>Send</button>
  </footer>

  {#if approval}
    <div class="overlay">
      <div class="modal">
        <h3>Approval needed</h3>
        <p class="areason">{approval.reason}</p>
        <div class="afield"><span>action</span><code>{approval.command}</code></div>
        <div class="afield"><span>cwd</span><code>{approval.cwd}</code></div>
        {#if approval.details}
          <pre class="adetails">{approval.details}</pre>
        {/if}
        <div class="abtns">
          <button class="deny" onclick={() => decide(false)}>Deny</button>
          <button class="ok" onclick={() => decide(true)}>Approve</button>
        </div>
      </div>
    </div>
  {/if}

  {#if usageOpen}
    <div class="overlay">
      <div class="modal wide">
        <h3>Usage</h3>
        <div class="usage-grid">
          <div class="usage-card">
            <strong>Last turn</strong>
            {#if lastMetrics}
              <div class="usage-row"><span>Model calls</span><b>{lastMetrics.iterations}</b></div>
              <div class="usage-row"><span>Tool calls</span><b>{lastMetrics.tool_calls}</b></div>
              <div class="usage-row"><span>Stop</span><b>{lastMetrics.stop_reason}</b></div>
              <div class="usage-row"><span>Prompt tokens</span><b>{usageValue(lastMetrics.usage, "prompt_tokens")}</b></div>
              <div class="usage-row"><span>Completion tokens</span><b>{usageValue(lastMetrics.usage, "completion_tokens")}</b></div>
              <div class="usage-row"><span>Total tokens</span><b>{totalTokens(lastMetrics.usage)}</b></div>
              <div class="usage-row"><span>Cache hit</span><b>{usageValue(lastMetrics.usage, "prompt_cache_hit_tokens")}</b></div>
              <div class="usage-row"><span>Cache miss</span><b>{usageValue(lastMetrics.usage, "prompt_cache_miss_tokens")}</b></div>
            {:else}
              <p class="emptyline">No usage yet.</p>
            {/if}
          </div>
          <div class="usage-card">
            <strong>Session</strong>
            <div class="usage-row"><span>Model calls</span><b>{sessionModelCalls}</b></div>
            <div class="usage-row"><span>Tool calls</span><b>{sessionToolCalls}</b></div>
            <div class="usage-row"><span>Prompt tokens</span><b>{usageValue(sessionUsage, "prompt_tokens")}</b></div>
            <div class="usage-row"><span>Completion tokens</span><b>{usageValue(sessionUsage, "completion_tokens")}</b></div>
            <div class="usage-row"><span>Total tokens</span><b>{totalTokens(sessionUsage)}</b></div>
            <div class="usage-row"><span>Cache hit</span><b>{usageValue(sessionUsage, "prompt_cache_hit_tokens")}</b></div>
            <div class="usage-row"><span>Cache miss</span><b>{usageValue(sessionUsage, "prompt_cache_miss_tokens")}</b></div>
          </div>
          <div class="usage-card">
            <strong>Context edit</strong>
            {#if lastMetrics}
              <div class="usage-row"><span>Original chars</span><b>{lastMetrics.context_edit.original_chars}</b></div>
              <div class="usage-row"><span>Edited chars</span><b>{lastMetrics.context_edit.edited_chars}</b></div>
              <div class="usage-row"><span>Saved chars</span><b>{savedContextChars(lastMetrics.context_edit)}</b></div>
              <div class="usage-row"><span>Compressed tools</span><b>{lastMetrics.context_edit.compressed_tool_results}</b></div>
              <div class="usage-row"><span>Dropped messages</span><b>{lastMetrics.context_edit.dropped_messages}</b></div>
            {:else}
              <p class="emptyline">No context edit yet.</p>
            {/if}
            <div class="usage-row"><span>Session compressed</span><b>{sessionCompressedToolResults}</b></div>
            <div class="usage-row"><span>Session dropped</span><b>{sessionDroppedMessages}</b></div>
          </div>
        </div>
        {#if lastMetrics?.tools_used.length}
          <div class="usage-tools">
            {#each lastMetrics.tools_used as tool}<code>{tool}</code>{/each}
          </div>
        {/if}
        <div class="abtns">
          <button class="plain" onclick={resetUsage}>Reset</button>
          <button class="deny" onclick={() => (usageOpen = false)}>Close</button>
        </div>
      </div>
    </div>
  {/if}

  {#if checkpointOpen}
    <div class="overlay">
      <div class="modal">
        <h3>Checkpoints</h3>
        <div class="checkpoint-create">
          <input bind:value={checkpointLabel} placeholder="Label" />
          <button onclick={saveCheckpoint} disabled={checkpointBusy}>Save</button>
          <button class="plain" onclick={loadCheckpoints} disabled={checkpointBusy}>Refresh</button>
        </div>
        <div class="checkpoint-list">
          {#if checkpoints.length === 0}
            <p class="emptyline">No checkpoints.</p>
          {/if}
          {#each checkpoints as cp}
            <div class="checkpoint-row">
              <div class="checkpoint-main">
                <strong>{cp.label || "(unlabeled)"}</strong>
                <code>{cp.id}</code>
              </div>
              <div class="checkpoint-meta">
                <span>{cp.created_at}</span>
                <span>{cp.files} files</span>
                <span>{cp.skipped} skipped</span>
              </div>
              <button class="restore" onclick={() => restoreCheckpoint(cp.id)} disabled={busy || checkpointBusy}>
                Restore
              </button>
            </div>
          {/each}
        </div>
        <div class="abtns">
          <button class="deny" onclick={() => (checkpointOpen = false)}>Close</button>
        </div>
      </div>
    </div>
  {/if}

  {#if commandOpen}
    <div class="overlay">
      <div class="modal">
        <h3>Commands</h3>
        <div class="command-actions">
          <button class="plain" onclick={loadCommands} disabled={commandBusy}>Refresh</button>
        </div>
        <div class="command-list">
          {#if customCommands.length === 0}
            <p class="emptyline">No custom commands.</p>
          {/if}
          {#each customCommands as cmd}
            <div class="command-row">
              <div class="command-main">
                <strong>{cmd.slash}</strong>
                <code title={cmd.path}>{cmd.path}</code>
              </div>
              <input bind:value={commandArgs[cmd.slash]} placeholder="Arguments" disabled={busy || commandBusy} />
              <button class="restore" onclick={() => runCustomCommand(cmd)} disabled={busy || commandBusy}>
                Run
              </button>
            </div>
          {/each}
        </div>
        <div class="abtns">
          <button class="deny" onclick={() => (commandOpen = false)}>Close</button>
        </div>
      </div>
    </div>
  {/if}

  {#if memoryOpen}
    <div class="overlay">
      <div class="modal wide">
        <h3>Memory</h3>
        {#if memory}
          <div class="memory-path">
            <span>{memory.count} notes</span>
            <code title={memory.path}>{memory.path}</code>
          </div>
        {/if}
        <div class="memory-create">
          <textarea bind:value={memoryNote} rows="3" placeholder="Verified note"></textarea>
          <input bind:value={memoryTags} placeholder="tags" />
          <button onclick={saveMemoryNote} disabled={memoryBusy || memoryNote.trim() === ""}>Save</button>
        </div>
        <div class="memory-actions">
          <button class="plain" onclick={loadMemory} disabled={memoryBusy}>Refresh</button>
          <button class="plain" onclick={openMemoryFile} disabled={memoryBusy}>Open file</button>
          <button class="plain" onclick={() => mergeMemory("heuristic")} disabled={memoryBusy}>Merge</button>
          <button class="restore" onclick={() => mergeMemory("llm")} disabled={memoryBusy}>LLM merge</button>
        </div>
        <div class="memory-list">
          {#if !memory || memory.entries.length === 0}
            <p class="emptyline">No memory notes.</p>
          {/if}
          {#each memory?.entries ?? [] as entry}
            <div class="memory-row">
              <div class="memory-text">{entry.text}</div>
              <div class="memory-meta">
                <span>{formatMemoryTime(entry.ts)}</span>
                {#if entry.tags.length > 0}<code>{entry.tags.join(", ")}</code>{/if}
              </div>
            </div>
          {/each}
        </div>
        <div class="abtns">
          <button class="deny" onclick={() => (memoryOpen = false)}>Close</button>
        </div>
      </div>
    </div>
  {/if}

  {#if settings}
    <div class="overlay">
      <div class="modal">
        <h3>Settings</h3>
        {#if configLocation}
          <div class="config-entry">
            <span>Config</span>
            <code title={configLocation.config_path}>{configLocation.config_path}</code>
            <button class="plain" onclick={openConfigFile}>Open file</button>
            <button class="plain" onclick={openConfigDir}>Open folder</button>
          </div>
        {/if}
        <label>
          <span>Model</span>
          <select bind:value={settings.model}>
            {#each settings.available_models as m}<option value={m}>{m}</option>{/each}
          </select>
        </label>
        <label>
          <span>Sandbox</span>
          <select bind:value={settings.sandbox_mode}>
            {#each settings.sandbox_modes as s}<option value={s}>{s}</option>{/each}
          </select>
        </label>
        <label>
          <span>Approval</span>
          <select bind:value={settings.approval_policy}>
            {#each settings.approval_policies as a}<option value={a}>{a}</option>{/each}
          </select>
        </label>
        <label>
          <span>Reasoning</span>
          <input bind:value={settings.reasoning_effort} placeholder="auto | low | medium | high | max | off" />
        </label>
        <label>
          <span>Model calls</span>
          <input type="number" min="1" bind:value={settings.max_iterations} />
        </label>
        <label>
          <span>Tool calls</span>
          <input type="number" min="0" bind:value={settings.max_tool_calls} />
        </label>
        <label class="check">
          <span>Context edit</span>
          <input type="checkbox" bind:checked={settings.context_edit_enabled} />
        </label>
        <label>
          <span>Context chars</span>
          <input type="number" min="1" bind:value={settings.context_edit_max_chars} />
        </label>
        <label>
          <span>Recent messages</span>
          <input type="number" min="1" bind:value={settings.context_edit_keep_recent_messages} />
        </label>
        <label>
          <span>Tool result chars</span>
          <input type="number" min="1" bind:value={settings.context_edit_max_tool_result_chars} />
        </label>
        <label>
          <span>Base URL</span>
          <input bind:value={settings.base_url} />
        </label>
        <label>
          <span>API key</span>
          <input
            type="password"
            bind:value={apiKeyInput}
            placeholder={settings.has_api_key ? `keep current (${settings.api_key_masked})` : "set an API key"}
          />
        </label>
        <div class="abtns">
          <button class="deny" onclick={() => (settings = null)}>Cancel</button>
          <button class="ok" onclick={saveSettings} disabled={saving}>
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  {/if}
</main>
