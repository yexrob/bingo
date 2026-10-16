// Generated from JSON Schema. Do not edit.
export type AbovePromptProps = { "bodyColumns": number; "hasSurvey": boolean; "isWorking": boolean; "maxRows": number; "scroll": SiteScroll; "view": SiteView; [key: string]: unknown; };
/** A typed action a client asks for (GUI buttons, hosts); the kernel dispatches it to a command. */
export type Action = { "args"?: unknown; "name": string; [key: string]: unknown; };
/** A button: the surface fires `Input::Action` with `action` (ADR-0008 §1). */
export type ActionItem = {
  "action": Action;
  /** A single-key hint; the surface may use another key. */
  "key"?: (string | null);
  "label": string;
  [key: string]: unknown;
};
export type ActionProps = { "args": unknown; "name": string; "result"?: unknown; "status": ItemStatus; [key: string]: unknown; };
export type After = { "chain": string; "result": SubmitResult; [key: string]: unknown; };
export type After10 = { "chain": string; "result": Catalog; [key: string]: unknown; };
export type After11 = { "chain": string; "result": null; [key: string]: unknown; };
export type After12 = { "chain": string; "result": Input; [key: string]: unknown; };
export type After13 = { "chain": string; "result": Append; [key: string]: unknown; };
export type After14 = { "chain": string; "result": CommandReply; [key: string]: unknown; };
export type After15 = { "chain": string; "result"?: (CommandSpec | null); [key: string]: unknown; };
export type After16 = { "chain": string; "result": ConfigSet; [key: string]: unknown; };
export type After17 = { "chain": string; "result": ConfigView; [key: string]: unknown; };
export type After18 = { "chain": string; "result": unknown; [key: string]: unknown; };
export type After2 = { "chain": string; "result": CheckResult; [key: string]: unknown; };
export type After3 = { "chain": string; "result": ToolCallAnswer; [key: string]: unknown; };
export type After4 = { "chain": string; "result": _Result_5f_of_5f_Array_5f_of_5f_ModelEvent_5f_or_5f_ProviderError; [key: string]: unknown; };
export type After5 = { "chain": string; "result": ModelRequest; [key: string]: unknown; };
export type After6 = { "chain": string; "result": _Result_5f_of_5f_Compaction_5f_or_5f_CompactError; [key: string]: unknown; };
export type After7 = { "chain": string; "result": StopResult; [key: string]: unknown; };
export type After8 = { "chain": string; "result"?: (string | null); [key: string]: unknown; };
export type After9 = { "chain": string; "result"?: (ToolSpec | null); [key: string]: unknown; };
export type Align = ("flex-start" | "center" | "flex-end" | "stretch");
export type Ancestors = {
  /** One file or directory name, without a separator. */
  "name": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type Anchor = ("topLeft" | "topRight" | "bottomLeft" | "bottomRight" | "center");
export type Answer = ({ "input": unknown; "kind": "allowChanged"; [key: string]: unknown; } | { "kind": "allowOnce"; [key: string]: unknown; } | { "kind": "allowSession"; "scope": string; [key: string]: unknown; } | { "feedback"?: (string | null); "kind": "deny"; [key: string]: unknown; } | { "ids": Array<string>; "kind": "choice"; "other"?: (string | null); [key: string]: unknown; } | { "kind": "text"; "text": string; [key: string]: unknown; } | { "kind": "confirm"; [key: string]: unknown; } | { "kind": "cancel"; [key: string]: unknown; } | { "answers": Array<Answer>; "kind": "form"; [key: string]: unknown; });
/**
 * The two options a session may answer a question with when nobody is asked
 * (ADR-0039 §2). The asker names them in its own words; the kernel reads
 * which is which and never the words.
 */
export type AnswerRole = ("allowing" | "refusing");
export type AnswerSpec = ("allowChanged" | "allowOnce" | "allowSession" | "deny" | "choice" | "text" | "confirm" | "cancel" | "form");
export type Append = {
  /** The record's kind, named by its plugin. */
  "kind": string;
  /** The record itself. */
  "payload": unknown;
  /** The plugin that owns the record. */
  "plugin": string;
  [key: string]: unknown;
};
export type Approval = { "root": string; "what": string; };
export type ArgSpec = ({ "kind": "none"; [key: string]: unknown; } | { "args": ArgSpec; "flags": Array<string>; "kind": "options"; [key: string]: unknown; } | { "hint": string; "kind": "free"; [key: string]: unknown; } | { "kind": "catalog"; "source": string; [key: string]: unknown; } | {
  /** Words among `values` that end the argument; `then` does not apply. */
  "alone"?: Array<string>;
  "kind": "words";
  /**
   * What a surface shows for a value that is not its own best name;
   * the line still runs the value, and a value without one reads as
   * itself.
   */
  "labels"?: { [key: string]: string; };
  /**
   * Words among `values` only the person may run: they raise trust or
   * let repository code run, so no link rewrites a line that starts
   * with one and no view bingo did not draw may send one.
   */
  "person"?: Array<string>;
  "then"?: (ArgSpec | null);
  "values": Array<string>;
  [key: string]: unknown;
});
/**
 * `bingo.host.ask`: a question from the process, on one of its running calls.
 *
 * The call names the crossing and is the whole of the grant: the bridge
 * already tracks running calls, an ended one is not there to ask on, and
 * another connection's is not this caller's to reach (ADR-0033 §3 as
 * amended). Nothing is minted for it.
 *
 * The question is the sdk's own `InteractionKind`, so the person is asked by
 * the machinery every in-process tool's question rides. Only a question
 * crosses: a permission, a confirmation or a login is the host's own to open,
 * and a grant is never Allow-shaped.
 */
export type AskParams = {
  /** The `callId` this process was given in `tool/call`. */
  "call": string;
  "question": InteractionKind;
  [key: string]: unknown;
};
/**
 * What the person answered, as the sdk writes it — including the cancel that
 * is an answer.
 */
export type AskResult = { "answer": Answer; [key: string]: unknown; };
export type AssetProps = { "asset": string; "label"?: (string | null); [key: string]: unknown; };
export type AssistantMessageProps = { "isFirstOfReply": boolean; "text": string; [key: string]: unknown; };
export type Authorize = {
  /** The answers the person may give. */
  "answers": Array<AnswerSpec>;
  /** What kind of question it is. */
  "kind": InteractionKind;
  "session": SessionId;
  [key: string]: unknown;
};
export type Before = { "chain": string; "input": Input; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before10 = { "chain": string; "input": Append; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before11 = { "chain": string; "input": CommandRun; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before12 = { "chain": string; "input": CommandSpec; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before13 = { "chain": string; "input": ConfigSet; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before14 = { "chain": string; "input": ConfigView; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before15 = { "chain": string; "input": unknown; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before2 = { "chain": string; "input": ToolEvent; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before3 = { "chain": string; "input": ModelRequest; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before4 = { "chain": string; "input": CompactInput; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before5 = { "chain": string; "input": null; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before6 = { "chain": string; "input": Turn; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before7 = { "chain": string; "input": ToolSpec; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before8 = { "chain": string; "input": Catalog; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type Before9 = { "chain": string; "input": Send; "site": HookSite; "tier": Tier; [key: string]: unknown; };
export type BrowseProps = { "action": Action; "busy": boolean; "confirm": boolean; "count": number; "current": number; "hit"?: (Match | null); "length": number; "previewed": boolean; "query": string; "refresh": boolean; "selected"?: (TurnId | null); "session": SessionId; "turns": Array<Turn2>; [key: string]: unknown; };
export type CallbackParams = { "callback": string; "chain": string; "input": unknown; "site": HookSite; [key: string]: unknown; };
export type CallingHook = { "hook": number; "plugin": string; [key: string]: unknown; };
export type Cancel = { "chain": string; [key: string]: unknown; };
export type CancelReason = (("turnEnded" | "interrupted" | "sessionClosed" | "expired" | "superseded") | "commandEnded");
export type Catalog = { "entries": Array<CatalogEntry>; "kind": CatalogKind; [key: string]: unknown; };
export type CatalogEntry = { "id": string; "label": string; "meta"?: unknown; [key: string]: unknown; };
export type CatalogKind = ("events" | "models" | "providers" | "tools" | "commands" | "plugins");
export type CheckResult = {
  /** The call as judged, possibly with tightened arguments. */
  "call": ToolCall;
  /** Whether it may run. */
  "decision": Decision;
  [key: string]: unknown;
};
/** Whether the chosen model applies only here or is the default for new sessions. */
export type ChoiceScope = ("session" | "default");
export type Classify = {
  /** The labels the model chooses from. */
  "labels": Array<string>;
  "session": SessionId;
  /** The text to classify. */
  "text": string;
  "turn"?: (TurnId | null);
  [key: string]: unknown;
};
export type ClientIdentity = { "name": string; "surface": string; [key: string]: unknown; };
export type CloseReason = ({ "kind": "client"; [key: string]: unknown; } | { "kind": "shutdown"; [key: string]: unknown; } | { "kind": "deleted"; [key: string]: unknown; } | { "kind": "error"; "message": string; [key: string]: unknown; });
/** Commands presented through one destination while their dispatch stays intact. */
export type CommandFold = {
  /** Destination command shown in the palette for the folded names. */
  "into": string;
  /** Command names or aliases found through the destination without changing dispatch. */
  "names": Array<string>;
  [key: string]: unknown;
};
export type CommandOutcome = ({ "kind": "acknowledged"; "result": unknown; [key: string]: unknown; } | { "action": Action; "fallback"?: (string | null); "kind": "surface"; [key: string]: unknown; } | { "answer": Answer; "kind": "answer"; [key: string]: unknown; } | { "handled": boolean; "kind": "handled"; [key: string]: unknown; } | { "kind": "applied"; "message"?: (string | null); [key: string]: unknown; } | { "kind": "view"; "view": View; [key: string]: unknown; } | { "kind": "prompt"; "text": string; [key: string]: unknown; } | { "body": ItemBody; "kind": "record"; [key: string]: unknown; });
export type CommandOutputProps = { "args": string; "command": string; "isErrored": boolean; "text": string; [key: string]: unknown; };
export type CommandReply = (CommandOutcome | { "text": string; [key: string]: unknown; });
export type CommandRun = {
  /** The rest of the line after the name. */
  "args": string;
  /** The command's name, without the slash. */
  "command": string;
  /** The surface the command was typed on, when known. */
  "surface"?: (SurfaceName | null);
  [key: string]: unknown;
};
/** One `/name args` line, with the argument text exactly as it was typed. */
export type CommandRunParams = { "args": string; "cwd": string; "name": string; "session": SessionId; [key: string]: unknown; };
export type CommandRunResult = { "outcome": CommandOutcome; [key: string]: unknown; };
export type CommandSpec = {
  "aliases"?: Array<string>;
  "args": ArgSpec;
  "family": string;
  "hidden"?: boolean;
  /** A one-line description for the command menu; argument syntax belongs in `args`. */
  "hint": string;
  /** May run while a turn is busy (read-only commands). */
  "instant"?: boolean;
  "keys"?: Array<KeyBinding>;
  "name": string;
  /**
   * Run bare, it opens the place where its argument is chosen, so a
   * palette runs it rather than listing `args`; `args` stays for every
   * surface that completes it.
   */
  "opens"?: boolean;
  [key: string]: unknown;
};
export type CommandsChanged = { "commands": Array<CommandSpec>; [key: string]: unknown; };
/** A rejected summary still bills any usage observed before it failed. */
export type CompactError = { "error": KernelError; "usage"?: Usage; [key: string]: unknown; };
export type CompactInput = {
  /** Earlier failed attempts this round. */
  "failures": number;
  /** The history to compact. */
  "items": Array<Item>;
  /** Tokens of recent history to keep verbatim. */
  "keepBudget": number;
  /** Why compaction runs. */
  "reason": CompactReason;
  /** The request the next round would send. */
  "request": ModelRequest;
  /** The context reading that triggered it. */
  "usage": ContextUsage;
  [key: string]: unknown;
};
/**
 * Why a compaction was asked for. Serializable because a strategy may live in
 * another process: the reason crosses the bridge as it is.
 */
export type CompactReason = ({ "kind": "threshold"; [key: string]: unknown; } | { "kind": "overflow"; "message": string; [key: string]: unknown; } | { "instructions"?: (string | null); "kind": "manual"; [key: string]: unknown; });
/**
 * The result: a summary, the boundary before which items are replaced, the
 * items before it to keep anyway, and what the summary cost. The kernel
 * accepts it only when `after < before`; the cost is billed either way.
 */
export type Compaction = {
  "after": number;
  "before": number;
  "boundary": ItemId;
  "kept"?: Array<ItemId>;
  "summary": string;
  /** What the summary cost. A strategy that spends no tokens says nothing. */
  "usage"?: Usage;
  [key: string]: unknown;
};
export type CompactionProps = { "after": number; "before": number; "durationMs": number; "replaced": number; "summary": string; [key: string]: unknown; };
export type CompactorCompactParams = {
  "context": CompactorContext;
  /** Which of the compactors the handshake declared is being asked. */
  "id": string;
  "reason": CompactReason;
  [key: string]: unknown;
};
/**
 * Declared failures retain observed usage; generic transport errors cannot
 * report what a process spent before the connection failed.
 */
export type CompactorCompactResult = ({ "compaction": Compaction; "kind": "completed"; [key: string]: unknown; } | { "error": CompactError; "kind": "failed"; [key: string]: unknown; });
/**
 * What a compaction acts on, as a process that is not in the host can read
 * it: the sdk's `CompactContext` without what cannot cross — the host's
 * provider and the turn's cancellation token. A remote strategy summarises by
 * its own means, or cuts by none.
 */
export type CompactorContext = {
  "capabilities": ModelCapabilities;
  /**
   * Consecutive compactions the kernel discarded; at `BREAKER_TRIP` the
   * breaker is tripped and a strategy takes its rung that needs no model.
   */
  "failures": number;
  "items"?: Array<Item>;
  /** Tokens of the newest items a cut should leave intact. */
  "keepBudget": number;
  "request": ModelRequest;
  "usage": ContextUsage;
  [key: string]: unknown;
};
/**
 * A compaction strategy a process says it has. One field, and the same shape
 * as every other declaration: a plugin's answer to "what do you contribute"
 * reads the same way whatever the kind.
 */
export type CompactorSpec = { "id": string; [key: string]: unknown; };
export type Complete = { "request": ModelRequest; "session": SessionId; "turn"?: (TurnId | null); [key: string]: unknown; };
export type Completed = { "events": Array<ModelEvent>; [key: string]: unknown; };
export type Component = ("KeyBar" | "Composer" | "ModelPicker" | "Browse" | "Plugins" | "Palette" | "Sessions" | "Status" | "Question" | "UserMessage" | "AssistantMessage" | "ToolUse" | "ToolResult" | "ToolGroup" | "ToolProgress" | "CommandOutput" | "Spinner" | "TurnDuration" | "InfoNotice" | "SessionMode" | "PromptHint" | "AbovePrompt" | "Pane" | "Permission" | "SignIn" | "Reasoning" | "Compaction" | "Rewind" | "Interruption" | "QuestionAnswer" | "PermissionReceipt" | "Asset" | "ShellRun" | "Action" | "TurnProgress" | "SessionStatus" | "Toast" | "Welcome");
export type ComposerProps = { "placeholder": string; "tag": string; [key: string]: unknown; };
export type ConfigSet = {
  /** The setting's dotted key. */
  "key": string;
  /** Its new value. */
  "value": unknown;
  [key: string]: unknown;
};
/** Configuration as a client sees it: the kernel's keys and each plugin's claimed slice. */
export type ConfigView = { "kernel"?: unknown; "plugins"?: { [key: string]: unknown; }; [key: string]: unknown; };
export type ContentPart = ({ "text": string; "type": "text"; [key: string]: unknown; } | Image | { "id": string; "input": unknown; "name": string; "type": "toolUse"; [key: string]: unknown; } | { "isError"?: boolean; "parts": Array<ContentPart>; "toolUseId": string; "type": "toolResult"; [key: string]: unknown; } | { "providerMetadata"?: { [key: string]: { [key: string]: unknown; }; }; "text": string; "type": "reasoning"; [key: string]: unknown; });
export type ContextContributeParams = {
  /** Which of the contributors the handshake declared is being asked. */
  "id": string;
  "query": ContributeQuery;
  [key: string]: unknown;
};
export type ContextContributeResult = {
  /**
   * A contributor with nothing to add this round says so with an empty
   * list, which is never wrong.
   */
  "pieces"?: Array<ContextPiece>;
  [key: string]: unknown;
};
export type ContextKind = ("system" | "tools" | "chat" | "pictures");
export type ContextPart = { "name": ContextKind; "tokens": number; [key: string]: unknown; };
/**
 * What a contributor adds. Serializable for the same reason a placement is:
 * a piece written in another process crosses as it is, never as a copy of
 * itself the bridge invented.
 */
export type ContextPiece = (SystemBlock | { "kind": "user"; "label": string; "parts": Array<ContentPart>; [key: string]: unknown; });
/**
 * The one ruler for context: used tokens, the model window, and the point at
 * which compaction triggers.
 */
export type ContextUsage = {
  /** Approximate local request composition, independent of server anchoring. */
  "parts"?: (Array<ContextPart> | null);
  /**
   * `used` at which the older turns are summarised, equal to `window`
   * where the kernel draws no line because the endpoint holds the context
   * and cuts it itself (ADR-0055 §2).
   */
  "trigger": number;
  /** Tokens the next request would carry, anchored on the server's last count. */
  "used": number;
  /**
   * The input side of the model's window: what is left once the output
   * budget is reserved (ADR-0006).
   */
  "window": number;
  [key: string]: unknown;
};
/**
 * One round, as a process that is not in the host can read it.
 *
 * The serializable projection of the sdk's `ContextQuery` (ADR-0030 §5): the
 * session, where it is and what it has said so far. The host handle the
 * in-process query carries stays on this side — an external contributor
 * reads the round, it does not reach the host.
 */
export type ContributeQuery = { "capabilities": ModelCapabilities; "cwd": string; "items"?: Array<Item>; "round": number; "session": SessionSummary; "turn": TurnId; "usage": ContextUsage; [key: string]: unknown; };
/**
 * A contributor a process says it has. Placement is handshake data: which of
 * the three moments this contributor speaks at is asked once, here, and never
 * per call. A placement this host does not know refuses the handshake, in
 * words, rather than being guessed at.
 */
export type ContributorSpec = {
  /**
   * The name this process knows the contributor by; `context/contribute`
   * carries it back. The kernel sees it prefixed by the plugin's own name.
   */
  "id": string;
  "placement": Placement;
  [key: string]: unknown;
};
export type Decision = ({ "kind": "allow"; "reason": Reason; [key: string]: unknown; } | { "kind": "deny"; "reason": Reason; [key: string]: unknown; } | {
  "kind": "ask";
  "reason": Reason;
  /** The narrowest rule that would silence this prompt, offered as "allow for session". */
  "scope"?: (string | null);
  [key: string]: unknown;
});
export type DecisionKind = ("allowChanged" | "allowChangedSession" | "allow" | "allowSession" | "deny");
export type Deliver = { "input": Input; "session": SessionId; "wake": boolean; [key: string]: unknown; };
/**
 * How a line reaches a session's queue: a peer's message (ADR-0010 §1) and
 * a person's own line are the same question, so they are the same word.
 */
export type Delivery = ("wake" | "hold");
export type DeltaKind = ("text" | "reasoning" | "tail");
export type Direction = ("row" | "column");
export type DollarData = { "plugin": PluginData; [key: string]: unknown; };
/** What a session does with what it is told (ADR-0011 §1). */
export type Driver = ("model" | "log");
/** Reasoning effort, in the vocabulary providers converge on. */
export type Effort = ("minimal" | "low" | "medium" | "high" | "xHigh" | "max");
export type Encoding = ("utf8" | "base64");
/**
 * What an endpoint does with a request, as only the provider can know:
 * whether image parts reach the model, whether tokens can be counted ahead,
 * whether prefixes are cached, whether the conversation is its own. The
 * model's own facts — window, output budget, reasoning, vision — are the
 * kernel catalogue's (ADR-0004).
 */
export type EndpointCapabilities = {
  "caching"?: boolean;
  /**
   * The window this endpoint last named for this model. A server's word on
   * itself outranks every guess at it (ADR-0055 §2).
   */
  "contextWindow"?: (number | null);
  "countTokens"?: boolean;
  "images"?: boolean;
  [key: string]: unknown;
};
/** The process to spawn. */
export type Entry = {
  "args"?: Array<string>;
  "command": string;
  /** Added to the host's environment, never replacing it. */
  "env"?: { [key: string]: string; };
  [key: string]: unknown;
};
export type EnvGet = {
  /** The variable's name. */
  "name": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type EnvSet = {
  /** The variable's name. */
  "name": string;
  "session": SessionId;
  /** Its value. */
  "value": string;
  [key: string]: unknown;
};
export type ErrorCode = (("SESSION_NOT_FOUND" | "SESSION_LOCKED" | "SESSION_CLOSED" | "INTERACTION_CLOSED" | "NOT_READY" | "STALE_GENERATION" | "NOT_INITIALIZED" | "INVALID_INPUT" | "PERMISSION_DENIED" | "TOOL_NOT_FOUND" | "TOOL_FAILED" | "PROVIDER_UNAVAILABLE" | "AUTH_REQUIRED" | "RATE_LIMITED" | "CONTEXT_OVERFLOW" | "TIMEOUT" | "OFFLINE" | "SERVER_ERROR" | "TURN_LOST" | "TURN_BUDGET_EXHAUSTED" | "UNSUPPORTED" | "INTERNAL") | "INTERRUPTED" | "PROTOCOL_LIMIT" | "NOT_FOUND" | "STORAGE");
export type Event = ({ "summary": SessionSummary; "type": "sessionUpdated"; [key: string]: unknown; } | { "reason": CloseReason; "type": "sessionClosed"; [key: string]: unknown; } | { "inputs": Array<ItemId>; "origin": TurnOrigin; "turn": TurnId; "type": "turnStarted"; [key: string]: unknown; } | { "attempt": number; "delayMs": number; "dropped": Array<ItemId>; "max": number; "reason": string; "turn": TurnId; "type": "turnRetrying"; [key: string]: unknown; } | { "context": ContextUsage; "turn": TurnId; "type": "turnUsage"; "usage": Usage; [key: string]: unknown; } | { "context"?: (ContextUsage | null); "type": "contextChanged"; [key: string]: unknown; } | { "plugin": string; "turn"?: (TurnId | null); "type": "usageCharged"; "usage": Usage; [key: string]: unknown; } | { "status": TurnStatus; "turn": TurnId; "type": "turnCompleted"; "usage": Usage; [key: string]: unknown; } | { "item": Item; "type": "itemStarted"; [key: string]: unknown; } | { "data": string; "item": ItemId; "kind": DeltaKind; "n": number; "type": "itemDelta"; [key: string]: unknown; } | { "item": Item; "type": "itemUpdated"; [key: string]: unknown; } | { "item": Item; "type": "itemCompleted"; [key: string]: unknown; } | { "entries": Array<QueueEntry>; "revision": number; "type": "queueChanged"; [key: string]: unknown; } | { "interaction": Interaction; "type": "interactionOpened"; [key: string]: unknown; } | { "answer": Answer; "by": ResolvedBy; "id": InteractionId; "type": "interactionResolved"; [key: string]: unknown; } | { "id": InteractionId; "reason": CancelReason; "type": "interactionCancelled"; [key: string]: unknown; } | { "intent": IntentId; "outcome": IntentOutcome; "type": "intentAck"; [key: string]: unknown; } | { "boundary": ItemId; "generation": number; "kept": Array<ItemId>; "summary": ItemId; "type": "compacted"; [key: string]: unknown; } | { "dropped": Array<ItemId>; "filesRestored": Array<string>; "generation": number; "toTurn": TurnId; "type": "rewound"; [key: string]: unknown; } | { "config": ConfigView; "type": "configChanged"; [key: string]: unknown; } | { "kind": string; "type": "catalogChanged"; [key: string]: unknown; } | { "code": string; "level": Level; "text": string; "type": "notice"; [key: string]: unknown; } | { "kind": string; "payload": unknown; "plugin": string; "type": "extension"; [key: string]: unknown; } | { "requests": Array<string>; "type": "invalidated"; [key: string]: unknown; } | { "kind": string; "payload": unknown; "plugin": string; "type": "signal"; [key: string]: unknown; } | { "from": Seq; "to": Seq; "type": "lagged"; [key: string]: unknown; } | { [key: string]: unknown; });
export type Extend = { "key": string; "session": SessionId; "value": unknown; [key: string]: unknown; };
export type Fetch = {
  /** The request body, as text. */
  "body"?: (string | null);
  /** Request headers. */
  "headers"?: ({ [key: string]: string; } | null);
  /** `GET` unless given. */
  "method"?: (string | null);
  "session": SessionId;
  /** The URL to fetch. */
  "url": string;
  [key: string]: unknown;
};
export type Fetched = {
  /** The body, as text. */
  "body": string;
  /** Response headers. */
  "headers": { [key: string]: string; };
  /** A note from the host about the fetch, if any. */
  "notice"?: (string | null);
  /** The HTTP status. */
  "status": number;
  /** Whether the body was cut at the host's limit. */
  "truncated": boolean;
  [key: string]: unknown;
};
export type FileList = {
  /** At most this many entries. */
  "limit"?: (number | null);
  /** The directory to list. */
  "path": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type FilePath = {
  /** Relative to the session's cwd, or absolute under a granted root. */
  "path": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type FileRead = {
  /** `utf8` (the default) or `base64`. */
  "encoding"?: (Encoding | null);
  /** Relative to the session's cwd, or absolute under a granted root. */
  "path": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type FileStat = {
  /** Whether it is a directory. */
  "directory": boolean;
  /** Whether it is a regular file. */
  "file": boolean;
  /** Size in bytes. */
  "size": number;
  [key: string]: unknown;
};
export type FileWrite = {
  /** The new contents. */
  "body": string;
  /** How `body` is encoded: `utf8` (the default) or `base64`. */
  "encoding"?: (Encoding | null);
  /** Relative to the session's cwd, or absolute under a granted root. */
  "path": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type FinishReason = { "raw"?: (string | null); "unified": UnifiedFinish; [key: string]: unknown; };
export type Fork = {
  /** The one-off question. */
  "prompt": string;
  "session": SessionId;
  "turn"?: (TurnId | null);
  [key: string]: unknown;
};
export type ForkLink = { "session": SessionId; "turn"?: (TurnId | null); [key: string]: unknown; };
export type Frame = {
  /** The client intent this frame answers or results from, when there is one. */
  "cause"?: (IntentId | null);
  "event": Event;
  "seq": Seq;
  "session": SessionId;
  "ts": string;
  [key: string]: unknown;
};
export type HistoryChunk = {
  "generation": number;
  "items": Array<Item>;
  "next"?: (ItemId | null);
  /**
   * This item alone cannot fit the requested page. Fetch it explicitly in
   * parts before following `next`, which excludes that item.
   */
  "oversized"?: (OversizedItem | null);
  [key: string]: unknown;
};
export type HistoryPage = {
  "before"?: (ItemId | null);
  /** Reject a stale page after a rewind/compaction rather than mixing views. */
  "generation"?: (number | null);
  "limit"?: number;
  /**
   * Optional byte budget for the whole JSON-RPC response; absent preserves
   * `limit == 0` meaning all, not an implicit bounded page.
   */
  "maxBytes"?: (number | null);
  [key: string]: unknown;
};
/** Kernel → plugin: one observation point happened. Nothing is awaited. */
export type HookObserveParams = ({ "payload": unknown; "point": "catalogue"; [key: string]: unknown; } | { "payload": { "items"?: Array<Item>; "phase": Phase; "turn": TurnId; [key: string]: unknown; }; "point": "turn"; [key: string]: unknown; } | { "payload": { "phase": Phase; [key: string]: unknown; }; "point": "compact"; [key: string]: unknown; } | { "payload": { "phase": Phase; [key: string]: unknown; }; "point": "session"; [key: string]: unknown; } | { "payload": { "frame": Frame; [key: string]: unknown; }; "point": "event"; [key: string]: unknown; });
export type HookPoint = ("submit" | "beforeTool" | "afterTool" | "stop" | "turn" | "compact" | "session" | "event");
/**
 * Where a hook is standing, as a process that is not in the host can read it.
 *
 * The serializable projection of the sdk's `HookContext` (ADR-0030 §5): which
 * session, which turn if any, where it works and what model answers it. The
 * host handle and the provider stay on this side — an external hook reads
 * where it is, it does not reach the host.
 */
export type HookSite = { "cwd": string; "model"?: (string | null); "session": SessionId; "turn"?: (TurnId | null); [key: string]: unknown; };
/**
 * A hook a process says it has, and the matcher it is claimed with.
 *
 * The matcher is the sdk's own, declared once: the kernel's cheap skip runs
 * on this side, so an event this matcher rules out never crosses the pipe at
 * all (ADR-0032 §1). A declaration that names no point wants every point, as
 * an in-process hook's empty matcher does.
 */
export type HookSpec = {
  /**
   * The name this process knows the hook by; `hook/observe` carry it back. The kernel sees it prefixed by the
   * plugin's own name.
   */
  "id": string;
  /** Empty wants every point. */
  "points"?: Array<HookPoint>;
  /** Exact tool name or a trailing `*` prefix for the tool points. */
  "tool"?: (string | null);
  [key: string]: unknown;
};
export type HostCallContext = { "callers"?: Array<CallingHook>; "hook"?: (number | null); "parent"?: (string | null); [key: string]: unknown; };
/**
 * Where the host lives, as a process that is not in it can read.
 *
 * A projection of the sdk's `Env`, which is not `Serialize`: a plugin that
 * keeps state of its own needs somewhere to put it, and `dataDir` is where.
 */
export type HostEnv = { "configDir": string; "dataDir": string; "home": string; [key: string]: unknown; };
/**
 * A picture handed to the model, already the shape the journal keeps and a
 * provider's request encodes: one struct, so a surface that reads a picture
 * off disk, a wire client that already holds the bytes, and a tool result
 * that returns one all produce the same thing.
 */
export type Image = {
  /** Base64 payload. */
  "data": string;
  "height"?: (number | null);
  "mediaType": string;
  /**
   * Where the same bytes are on this machine, when they are anywhere: a
   * picture read off a disk, or one a surface wrote before handing it on
   * (ADR-0052). A location, not a second copy — absent from the wire and
   * the journal when there is none, so a picture without one is the
   * bytes it always was.
   */
  "path"?: (string | null);
  "width"?: (number | null);
  [key: string]: unknown;
};
export type InfoNoticeProps = { "command"?: (string | null); "text": string; [key: string]: unknown; };
/** The handshake, sent once on a fresh process. */
export type InitializeParams = {
  "capabilities": Array<string>;
  /** This plugin's settings slice (`plugins.<name>`); `null` when unset. */
  "config"?: unknown;
  "env": HostEnv;
  "pluginName": string;
  /** The directory the manifest was read from, already resolved. */
  "pluginRoot": string;
  "pluginVersion"?: (string | null);
  "protocol": number;
  [key: string]: unknown;
};
/**
 * What the process says it is and what it contributes. Everything here is a
 * claim: the tools it names are registered untrusted (ADR-0015 §4).
 */
export type InitializeResult = {
  "capabilities"?: Array<string>;
  "commands"?: Array<CommandSpec>;
  "compactors"?: Array<CompactorSpec>;
  "contributors"?: Array<ContributorSpec>;
  "hooks"?: Array<HookSpec>;
  "links"?: Array<LinkSpec>;
  "name": string;
  /**
   * The major the process speaks. The host refuses any but its own, which
   * it can only do if the process says which one it is.
   */
  "protocol": number;
  "providers"?: Array<ProviderSpec>;
  /**
   * The services this process serves, by the key another plugin calls them
   * by. A key is global — it is what a consumer writes in `requires` — so
   * the declaration is a map and not a list: one answer per key.
   */
  "services"?: { [key: string]: ServiceSpec; };
  "subscriptions"?: Array<LinkMatcher>;
  "tools"?: Array<ToolSpec>;
  "version": string;
  [key: string]: unknown;
};
/**
 * The one submission entry. The kernel parses `/`, `!` and `@` in text and
 * decides turn, queue, steer or deliver; a client never chooses.
 */
export type Input = ({
  /**
   * Whether a running turn may be steered with it (ADR-0008 §2,
   * amended M68). `Wake` is the line every client has always sent, so
   * it is the default and nothing is written for it.
   */
  "delivery"?: Delivery;
  /**
   * The pictures the ask carries, in the order they reach the journal
   * (ADR-0040): exactly as they will be journaled, because a surface
   * resolves a picture and the kernel does no file I/O for input.
   */
  "images"?: Array<Image>;
  "kind": "text";
  "origin": Origin;
  "text": string;
  [key: string]: unknown;
} | {
  "action": Action;
  "kind": "action";
  /**
   * The door it came through. Absent, or a door that is not a person
   * surface, and the action is not the person's own: it fails closed.
   */
  "origin"?: (Origin | null);
  [key: string]: unknown;
});
/** A client-minted write id; the outcome arrives as `IntentAck`. */
export type IntentId = string;
export type IntentOutcome = ({ "kind": "turnStarted"; "turn": TurnId; [key: string]: unknown; } | { "kind": "queued"; "position": number; [key: string]: unknown; } | { "kind": "applied"; "result": unknown; [key: string]: unknown; } | { "error": KernelError; "kind": "rejected"; [key: string]: unknown; });
export type Interaction = {
  /** Exactly the answers the kernel will accept. */
  "answers": Array<AnswerSpec>;
  "expiresAt"?: (string | null);
  /** Keyboard approvals before this instant are rejected with `NOT_READY`. */
  "guardUntil"?: (string | null);
  "id": InteractionId;
  "item"?: (ItemId | null);
  "kind": InteractionKind;
  "openedAt": string;
  "session": SessionId;
  "turn"?: (TurnId | null);
  [key: string]: unknown;
};
/** A pending question or permission, answered once by id. */
export type InteractionId = string;
export type InteractionKind = ({
  "caution"?: (string | null);
  /** The call after hooks and interceptors, before this decision. */
  "input"?: unknown;
  "kind": "permission";
  "preview"?: (Preview | null);
  /** The rule `AllowSession` would install, when one would silence the prompt. */
  "sessionScope"?: (string | null);
  "summary": string;
  "tool": string;
  [key: string]: unknown;
} | Question | {
  "kind": "form";
  "questions": Array<Question>;
  /**
   * What the whole set is about, in the asker's own words: an MCP
   * server's elicitation says which server is asking. A set the model
   * opened has nothing to add to its own questions and names none.
   */
  "title"?: (string | null);
  [key: string]: unknown;
} | { "detail": string; "kind": "confirm"; "title": string; [key: string]: unknown; } | { "flow": LoginFlow; "kind": "login"; "provider": string; [key: string]: unknown; });
export type InterruptReason = ("userCancel" | "newInput" | "shutdown" | "budget");
export type InterruptionProps = { "marker": string; [key: string]: unknown; };
export type Invalidate = { "requests": Array<string>; "session": SessionId; [key: string]: unknown; };
export type Invoke = {
  /** The tool and its arguments. */
  "call": ToolCall;
  "session": SessionId;
  [key: string]: unknown;
};
export type Item = {
  "body": ItemBody;
  "completedAt"?: (string | null);
  "id": ItemId;
  /** The client intent that produced this item, when one did (user items). */
  "intent"?: (IntentId | null);
  "meta"?: { [key: string]: unknown; };
  "round"?: number;
  "startedAt": string;
  "status": ItemStatus;
  "turn"?: (TurnId | null);
  [key: string]: unknown;
};
export type ItemBody = ({ "kind": "user"; "origin": Origin; "parts": Array<ContentPart>; [key: string]: unknown; } | { "kind": "assistant"; "text": string; [key: string]: unknown; } | { "kind": "reasoning"; "providerMetadata"?: { [key: string]: { [key: string]: unknown; }; }; "text": string; [key: string]: unknown; } | { "callId": string; "durationMs"?: (number | null); "input": unknown; "kind": "toolCall"; "name": string; "output"?: (ToolOutput | null); "progress"?: (string | null); "quiet"?: boolean; [key: string]: unknown; } | { "command": string; "cwd": string; "exit"?: (number | null); "kind": "shell"; "output": string; [key: string]: unknown; } | { "args"?: unknown; "kind": "action"; "name": string; "result"?: unknown; [key: string]: unknown; } | { "after": number; "before": number; "durationMs": number; "kind": "compaction"; "replaced": number; "summary": string; [key: string]: unknown; } | { "dropped": number; "kind": "rewind"; "toTurn": TurnId; [key: string]: unknown; } | { "kind": "interruption"; "marker": string; [key: string]: unknown; } | { "code": string; "kind": "notice"; "level": Level; "text": string; [key: string]: unknown; } | { "answer": string; "interaction": InteractionId; "item"?: (ItemId | null); "kind": "questionAnswer"; "question": string; [key: string]: unknown; } | { "decision": DecisionKind; "feedback"?: (string | null); "interaction": InteractionId; "item"?: (ItemId | null); "kind": "permissionReceipt"; "tool": string; [key: string]: unknown; } | { "asset": string; "kind": "asset"; "label"?: (string | null); [key: string]: unknown; } | { [key: string]: unknown; });
/** One unit in the transcript order. */
export type ItemId = string;
export type ItemStatus = ("pending" | "running" | "completed" | "failed" | "interrupted");
export type Justify = ("flex-start" | "center" | "flex-end" | "space-between" | "space-around" | "space-evenly");
/** An error as it appears on the wire and in the journal. */
export type KernelError = { "code": ErrorCode; "message": string; [key: string]: unknown; };
export type KeyBarProps = { "pairs": Array<KeyPair>; "toast"?: (KeyToast | null); [key: string]: unknown; };
/**
 * A chord a command declares for itself. A bare chord runs the command
 * alone; one with `args` submits the command with them, so a shortcut for
 * `/model next` is a binding on `model` and not a second catalogue entry.
 */
export type KeyBinding = (string | { "args": string; "chord": string; [key: string]: unknown; });
export type KeyPair = { "action": string; "emphasis"?: boolean; "keys": string; [key: string]: unknown; };
export type KeyToast = { "about"?: (string | null); "level": Level; "text": string; [key: string]: unknown; };
/**
 * A length: a number of terminal cells, or a share of the screen written as
 * a percentage string such as `'40%'`.
 */
export type Length = (number | string);
export type Level = ("info" | "warn" | "error");
export type LinkMatcher = {
  "patterns"?: Array<Pattern>;
  "prefilter"?: unknown;
  /** Per-hook render partials, when one link contains several hooks. */
  "render"?: (Array<unknown> | null);
  [key: string]: unknown;
};
export type LinkPoint = ("ui.press" | "ui.input" | "ui.select" | "ui.render" | "plugin.register" | "prompt.submit" | "tool.check" | "tool.call" | "model.complete" | "turn.step" | "prompt.compose" | "session.compact" | "turn.stop" | "tool.describe" | "prompt.autocomplete" | "session.send" | "session.receive" | "session.append" | "turn.complete" | "command.run" | "command.describe" | "config.set" | "config.describe" | "turn.start" | "session.measure" | "session.start" | "session.end" | "session.attach" | "session.detach");
export type LinkSpec = { "after"?: boolean; "point": LinkPoint; "tool"?: (string | null); [key: string]: unknown; };
export type LoginFlow = ({ "kind": "browser"; "url": string; [key: string]: unknown; } | { "code": string; "kind": "device"; "url": string; [key: string]: unknown; } | { "kind": "paste"; [key: string]: unknown; });
/**
 * What one directory under `plugins/` declares about itself.
 *
 * Unknown keys are kept rather than refused: a manifest written for a later
 * host must still start on this one.
 */
export type Manifest = ({
  /** Maximum seconds for tool/call and command/run; absent allows ten minutes. */
  "callTimeout"?: (number | null);
  /**
   * A JSON Schema for this plugin's own settings — the slice a person
   * writes under `plugins.<name>`, which reaches the process as
   * `initialize.config`. It is what a person reads; nothing in this
   * workspace validates a document against a schema.
   */
  "config"?: unknown;
  "entry": Entry;
  "module"?: never;
  /**
   * What the plugin calls itself. It must be the name of the directory
   * holding it: the path is what one layer overrides another by, so two
   * spellings of the name would be two answers to "which plugin is this".
   */
  "name": string;
  "uses"?: Array<string>;
  "version": string;
  [key: string]: unknown;
} | {
  /** Maximum seconds for tool/call and command/run; absent allows ten minutes. */
  "callTimeout"?: (number | null);
  /**
   * A JSON Schema for this plugin's own settings — the slice a person
   * writes under `plugins.<name>`, which reaches the process as
   * `initialize.config`. It is what a person reads; nothing in this
   * workspace validates a document against a schema.
   */
  "config"?: unknown;
  "entry"?: never;
  "module": string;
  /**
   * What the plugin calls itself. It must be the name of the directory
   * holding it: the path is what one layer overrides another by, so two
   * spellings of the name would be two answers to "which plugin is this".
   */
  "name": string;
  "uses"?: Array<string>;
  "version": string;
  [key: string]: unknown;
});
export type Match = { "item": ItemId; "line": number; "turn"?: (TurnId | null); [key: string]: unknown; };
export type Message = { "parts": Array<ContentPart>; "providerOptions"?: { [key: string]: { [key: string]: unknown; }; }; "role": Role; [key: string]: unknown; };
export type Messages = {
  /** Which page of history. */
  "page": HistoryPage;
  "session": SessionId;
  [key: string]: unknown;
};
export type Millis = {
  /** Milliseconds. */
  "ms": number;
  [key: string]: unknown;
};
/**
 * What a turn may assume about its model: the kernel's resolution of the
 * user's settings, the catalogue, the server's corrections and the
 * endpoint's facts (ADR-0004). The ruler and the gate read this.
 */
export type ModelCapabilities = { "caching"?: boolean; "contextWindow": number; "countTokens"?: boolean; "images"?: boolean; "maxOutput": number; "reasoning"?: boolean; [key: string]: unknown; };
/** Provider stream events. Never published; the accumulator folds them into items. */
export type ModelEvent = ({ "type": "streamStart"; "warnings"?: Array<string>; [key: string]: unknown; } | { "id"?: (string | null); "model"?: (string | null); "type": "responseMetadata"; [key: string]: unknown; } | { "id": string; "type": "textStart"; [key: string]: unknown; } | { "delta": string; "id": string; "type": "textDelta"; [key: string]: unknown; } | { "id": string; "type": "textEnd"; [key: string]: unknown; } | { "id": string; "type": "reasoningStart"; [key: string]: unknown; } | { "delta": string; "id": string; "type": "reasoningDelta"; [key: string]: unknown; } | { "id": string; "providerMetadata"?: { [key: string]: { [key: string]: unknown; }; }; "type": "reasoningEnd"; [key: string]: unknown; } | { "id": string; "name": string; "type": "toolInputStart"; [key: string]: unknown; } | { "delta": string; "id": string; "type": "toolInputDelta"; [key: string]: unknown; } | { "id": string; "type": "toolInputEnd"; [key: string]: unknown; } | { "id": string; "input": string; "name": string; "type": "toolCall"; [key: string]: unknown; } | { "type": "context"; "used": number; "window": number; [key: string]: unknown; } | { "after": number; "before": number; "type": "compacted"; [key: string]: unknown; } | { "finishReason": FinishReason; "type": "finish"; "usage": Usage; [key: string]: unknown; });
export type ModelInfo = { "display"?: (string | null); "id": string; [key: string]: unknown; };
export type ModelPickerProps = {
  "effort"?: (Effort | null);
  "filter": string;
  "scope": ModelScope;
  /** The chosen row's key; its position is found again on every drawing. */
  "selected"?: (string | null);
  "sheet"?: (string | null);
  [key: string]: unknown;
};
export type ModelRequest = {
  "maxTokens": number;
  "messages": Array<Message>;
  "model": string;
  "providerOptions"?: { [key: string]: { [key: string]: unknown; }; };
  "reasoning"?: (Effort | null);
  /**
   * Whose turn this is. A stateless provider ignores it; one that keeps a
   * conversation of its own per session — an ACP adapter holding one agent
   * session per bingo session (ADR-0035 §3), a stateful wire like the
   * Responses API — has no other way to know which conversation it is
   * answering. `None` is a request built by hand or a side question, which
   * belongs to no session.
   */
  "session"?: (SessionId | null);
  "system": Array<SystemBlock>;
  "tools": Array<ToolSpec>;
  [key: string]: unknown;
};
export type ModelScope = ("session" | "keep");
export type NextStart = ("nextStart" | "afterTurn");
/**
 * `bingo.host.notice`: one line for the person, under the plugin's own name.
 *
 * The one door that is scoped by nothing (ADR-0033 §4): it spends nothing but
 * a line, so it needs no crossing to belong to. The level is the kernel's own
 * three and nothing else can be written; an omitted one is the quietest.
 */
export type NoticeParams = { "level"?: Level; "message": string; [key: string]: unknown; };
/** A notice is told, not asked: there is nothing to answer with. */
export type NoticeResult = { [key: string]: unknown; };
/** Who submitted the input, as every surface records it. */
export type Origin = {
  "conversation"?: (string | null);
  "principal"?: (string | null);
  /** Drawing family of the caller, independent of its attribution identifier. */
  "renderSurface"?: (SurfaceName | null);
  "surface": string;
  "voice"?: (Voice | null);
  [key: string]: unknown;
};
/**
 * The actor's factual observation; only the RPC server can add a
 * connection-scoped availability token after pinning an immutable Item.
 */
export type OversizedItem = {
  /** FNV-1a 64 of the exact serialized Item UTF-8, lowercase hex. */
  "checksum": string;
  "id": ItemId;
  "totalBytes": number;
  [key: string]: unknown;
};
export type PaletteOpen = ("commands" | "keys" | "sessions");
export type PaletteProps = {
  "action": Action;
  "bindings": { [key: string]: string; };
  /**
   * Only the catalogues the query's command completes from, once the
   * query reaches its arguments: every press carries them, and a model
   * catalogue is the largest thing it would otherwise carry.
   */
  "catalogs": { [key: string]: Array<CatalogEntry>; };
  "commands": Array<CatalogEntry>;
  "conversation": Array<PaletteSession>;
  "keyHints": { [key: string]: string; };
  "open": PaletteOpen;
  "query": string;
  "session": SessionId;
  "sessions": Array<PaletteSession>;
  [key: string]: unknown;
};
/** A compact row, projected by the client from live or stored facts. */
export type PaletteSession = { "attention"?: (string | null); "detail": string; "id": SessionId; "label": string; [key: string]: unknown; };
/**
 * Where a pane sits: `'above'` the prompt, at the `'side'`, as an
 * `'overlay'` floating over the screen, or at a fixed `{rect}`.
 */
export type PanePlace = (("above" | "side" | "overlay") | { "rect": Rect; });
export type PaneProps = { "bodyColumns": number; "hints"?: (SiteHints | null); "isFocused": boolean; "placement": string; "scroll": SiteScroll; "title": string; "view": SiteView; [key: string]: unknown; };
/** A pane's height and width; each defaults when left out. */
export type PaneSize = {
  /** Width in columns. */
  "columns"?: Span;
  /** Height in rows. */
  "rows"?: Span;
  [key: string]: unknown;
};
/**
 * Where a child hangs in the tree: its parent, and the tool call that
 * spawned it when one did (ADR-0011 §3).
 */
export type ParentLink = { "item"?: (ItemId | null); "session": SessionId; [key: string]: unknown; };
export type Pattern = string;
export type PermissionProps = { "answers": Array<AnswerSpec>; "input": unknown; "interactionId": InteractionId; "tool": string; [key: string]: unknown; };
export type PermissionReceiptProps = { "decision": DecisionKind; "feedback"?: (string | null); "interaction": InteractionId; "tool": string; [key: string]: unknown; };
export type Phase = ("start" | "end");
export type Place = {
  "session"?: (SessionId | null);
  /** The plugin's placement key. Several independent slots can coexist. */
  "slot": string;
  "view"?: (Slot | null);
  [key: string]: unknown;
};
/**
 * When a contributor speaks. Serializable because a contributor may live in
 * another process: it declares its placement once, over the bridge.
 */
export type Placement = ({ "kind": "system"; "order": number; [key: string]: unknown; } | { "kind": "roundStart"; [key: string]: unknown; } | { "kind": "barrier"; [key: string]: unknown; });
export type PluginData = { "name": string; "root": string; "version"?: (string | null); [key: string]: unknown; };
export type PluginState = ({ "kind": "on"; } | { "kind": "off"; "why": string; } | { "kind": "failed"; "why": string; } | { "changed": boolean; "kind": "waiting"; } | { "kind": "pending"; } | { "at": NextStart; "kind": "pending"; "now": boolean; });
export type PluginsOpen = ("plugins" | "mcp");
export type PluginsProps = { "action": Action; "open": PluginsOpen; "plugins": Array<CatalogEntry>; "session": SessionId; [key: string]: unknown; };
export type Preview = ({ "kind": "diff"; "unified": string; [key: string]: unknown; } | { "command": string; "cwd": string; "description"?: (string | null); "kind": "command"; [key: string]: unknown; } | { "kind": "url"; "url": string; [key: string]: unknown; });
export type Proceed = ({ "next": Input; } | { "answer": SubmitResult; });
export type Proceed10 = ({ "next": Catalog; } | { "answer": Catalog; });
export type Proceed11 = ({ "next": Send; } | { "answer": null; });
export type Proceed12 = ({ "next": Input; } | { "answer": Input; });
export type Proceed13 = ({ "next": Append; } | { "answer": Append; });
export type Proceed14 = ({ "next": CommandRun; } | { "answer": CommandReply; });
export type Proceed15 = ({ "next": CommandSpec; } | { "answer": (CommandSpec | null); });
export type Proceed16 = ({ "next": ConfigSet; } | { "answer": ConfigSet; });
export type Proceed17 = ({ "next": ConfigView; } | { "answer": ConfigView; });
export type Proceed18 = ({ "next": unknown; } | { "answer": unknown; });
export type Proceed2 = ({ "next": ToolEvent; } | { "answer": CheckResult; });
export type Proceed3 = ({ "next": ToolEvent; } | { "answer": ToolCallAnswer; });
export type Proceed4 = ({ "next": ModelRequest; } | { "answer": _Result_5f_of_5f_Array_5f_of_5f_ModelEvent_5f_or_5f_ProviderError; });
export type Proceed5 = ({ "next": ModelRequest; } | { "answer": ModelRequest; });
export type Proceed6 = ({ "next": CompactInput; } | { "answer": _Result_5f_of_5f_Compaction_5f_or_5f_CompactError; });
export type Proceed7 = ({ "next": null; } | { "answer": StopResult; });
export type Proceed8 = ({ "next": Turn; } | { "answer": (string | null); });
export type Proceed9 = ({ "next": ToolSpec; } | { "answer": (ToolSpec | null); });
export type ProcessId = {
  /** The id `spawn` answered. */
  "id": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type ProcessInput = {
  /** The program and its arguments; no shell is involved. */
  "argv": Array<string>;
  /** Working directory; the session's cwd unless given. */
  "cwd"?: (string | null);
  "session": SessionId;
  /** Kill the process after this many milliseconds. */
  "timeoutMs"?: (number | null);
  [key: string]: unknown;
};
export type ProcessOutput = {
  /** The exit code, or `null` when killed by a signal. */
  "code"?: (number | null);
  /** Everything written to stderr. */
  "stderr": string;
  /** Everything written to stdout. */
  "stdout": string;
  [key: string]: unknown;
};
export type ProcessWrite = {
  /** Text for stdin; `null` closes it. */
  "body"?: (string | null);
  /** The id `spawn` answered. */
  "id": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type PromptHintProps = { "hint": string; "isDraft": boolean; "isWorking": boolean; [key: string]: unknown; };
export type PromptView = {
  /** The last submitted line. */
  "last"?: (string | null);
  /** Inputs waiting for the next turn. */
  "queue": Array<QueueEntry>;
  [key: string]: unknown;
};
export type Props = ({ "component": "KeyBar"; "props": KeyBarProps; [key: string]: unknown; } | { "component": "Composer"; "props": ComposerProps; [key: string]: unknown; } | { "component": "ModelPicker"; "props": ModelPickerProps; [key: string]: unknown; } | { "component": "Browse"; "props": BrowseProps; [key: string]: unknown; } | { "component": "Plugins"; "props": PluginsProps; [key: string]: unknown; } | { "component": "Palette"; "props": PaletteProps; [key: string]: unknown; } | { "component": "Sessions"; "props": SessionsProps; [key: string]: unknown; } | { "component": "Status"; "props": StatusProps; [key: string]: unknown; } | { "component": "Question"; "props": QuestionProps; [key: string]: unknown; } | { "component": "UserMessage"; "props": UserMessageProps; [key: string]: unknown; } | { "component": "AssistantMessage"; "props": AssistantMessageProps; [key: string]: unknown; } | { "component": "ToolUse"; "props": ToolUseProps; [key: string]: unknown; } | { "component": "ToolResult"; "props": ToolResultProps; [key: string]: unknown; } | { "component": "ToolGroup"; "props": ToolGroupProps; [key: string]: unknown; } | { "component": "ToolProgress"; "props": ToolProgressProps; [key: string]: unknown; } | { "component": "CommandOutput"; "props": CommandOutputProps; [key: string]: unknown; } | { "component": "Spinner"; "props": SpinnerProps; [key: string]: unknown; } | { "component": "TurnDuration"; "props": TurnDurationProps; [key: string]: unknown; } | { "component": "InfoNotice"; "props": InfoNoticeProps; [key: string]: unknown; } | { "component": "SessionMode"; "props": SessionModeProps; [key: string]: unknown; } | { "component": "PromptHint"; "props": PromptHintProps; [key: string]: unknown; } | { "component": "AbovePrompt"; "props": AbovePromptProps; [key: string]: unknown; } | { "component": "Pane"; "props": PaneProps; [key: string]: unknown; } | { "component": "Permission"; "props": PermissionProps; [key: string]: unknown; } | { "component": "SignIn"; "props": SignInProps; [key: string]: unknown; } | { "component": "Reasoning"; "props": ReasoningProps; [key: string]: unknown; } | { "component": "Compaction"; "props": CompactionProps; [key: string]: unknown; } | { "component": "Rewind"; "props": RewindProps; [key: string]: unknown; } | { "component": "Interruption"; "props": InterruptionProps; [key: string]: unknown; } | { "component": "QuestionAnswer"; "props": QuestionAnswerProps; [key: string]: unknown; } | { "component": "PermissionReceipt"; "props": PermissionReceiptProps; [key: string]: unknown; } | { "component": "Asset"; "props": AssetProps; [key: string]: unknown; } | { "component": "ShellRun"; "props": ShellRunProps; [key: string]: unknown; } | { "component": "Action"; "props": ActionProps; [key: string]: unknown; } | { "component": "TurnProgress"; "props": TurnProgressProps; [key: string]: unknown; } | { "component": "SessionStatus"; "props": SessionStatusProps; [key: string]: unknown; } | { "component": "Toast"; "props": ToastProps; [key: string]: unknown; } | { "component": "Welcome"; "props": WelcomeProps; [key: string]: unknown; });
/**
 * Kernel → plugin: nobody is reading this stream any more — the turn was
 * interrupted, or whoever asked let go. A process that ignores it is slow, not
 * broken: the host stops reading either way, and the stream's idle deadline is
 * what keeps a silent one from holding a turn open.
 */
export type ProviderCancelParams = { "call": string; [key: string]: unknown; };
/** Plugin → kernel, while a stream runs: one event, for one stream. */
export type ProviderDeltaParams = { "call": string; "event": ModelEvent; [key: string]: unknown; };
export type ProviderError = ({ "kind": "auth"; "message": string; [key: string]: unknown; } | { "kind": "rateLimited"; "retryAfterMs"?: (number | null); [key: string]: unknown; } | { "kind": "contextOverflow"; "message": string; [key: string]: unknown; } | { "kind": "server"; "message": string; "status": number; [key: string]: unknown; } | { "kind": "request"; "message": string; [key: string]: unknown; } | { "kind": "transport"; "message": string; [key: string]: unknown; } | { "kind": "stream"; "message": string; [key: string]: unknown; } | { "kind": "timeout"; [key: string]: unknown; } | { "kind": "unsupported"; "message": string; [key: string]: unknown; } | { "kind": "config"; "message": string; [key: string]: unknown; });
/**
 * A model provider a process says it serves, and the whole of what the host
 * believes about it. The id is the one a person types (`--provider <id>`,
 * `/model <id>/<model>`), `family` is the catalogue shelf its models are filed
 * under (ADR-0017; the id itself by default, as the sdk trait does), and
 * `models` is what it serves — the answer to `Provider::models`, given once
 * here so that asking costs no call.
 *
 * `endpoint` is what this endpoint does with a request for one of *those*
 * models. A model the declaration does not name gets nothing: no images, no
 * token counting, no caching (ADR-0015 §4).
 */
export type ProviderSpec = { "endpoint"?: EndpointCapabilities; "family"?: (string | null); "id": string; "models"?: Array<ModelInfo>; [key: string]: unknown; };
/**
 * One model response, asked for. The request crosses as the sdk writes it —
 * system blocks, messages, tools and all — because a provider that cannot read
 * the whole request cannot answer it.
 *
 * `call` names this stream. Every [`ProviderDeltaParams`] carries it back and
 * [`ProviderCancelParams`] stops it by it, so two streams running on one pipe
 * never interleave.
 */
export type ProviderStreamParams = {
  "call": string;
  /** Which of the providers the handshake declared is being asked. */
  "id": string;
  "request": ModelRequest;
  [key: string]: unknown;
};
/**
 * The close. A stream that ran to its `finish` event answers with nothing; one
 * that broke answers with the error the trait speaks, so the kernel's retry
 * ladder reads the same kind it reads from an in-process provider.
 */
export type ProviderStreamResult = { "error"?: (ProviderError | null); [key: string]: unknown; };
/** A placement has no asking surface until a client pulls it. */
export type PullRequest = { "component": Component; "props": unknown; "requestId": string; "viewport": Viewport; [key: string]: unknown; };
/**
 * One question put to a person: what is asked, what may be picked, and
 * whether words of one's own or more than one option count. A lone question
 * and one of a form are the same thing (M53).
 */
export type Question = {
  "freeText"?: boolean;
  /** A short tag a surface may show before the question (`Auth method`). */
  "header"?: (string | null);
  "multi"?: boolean;
  "options": Array<QuestionOption>;
  "question": string;
  [key: string]: unknown;
};
export type QuestionAnswerProps = { "answer": string; "interaction": InteractionId; "question": string; [key: string]: unknown; };
/** A question owner's editing projection; sealed labels remain the host's. */
export type QuestionFocus = { "checked": number; "field": number; "writing": boolean; [key: string]: unknown; };
export type QuestionOption = {
  "description"?: (string | null);
  "id": string;
  "label": string;
  /**
   * A few lines of monospace shown beside the option while the cursor is
   * on it — the layout or the snippet picking it would mean (M53).
   */
  "preview"?: (string | null);
  /**
   * What picking it would mean to a session that answers for the person
   * (ADR-0039 §2). Unmarked options are a person's alone.
   */
  "role"?: (AnswerRole | null);
  [key: string]: unknown;
};
export type QuestionProps = { "form"?: boolean; "interactionId"?: (InteractionId | null); "metadataSource"?: (string | null); "questions": Array<Question>; "tool": string; [key: string]: unknown; };
export type QueueEntry = { "intent": IntentId; "origin": Origin; "position": number; "preview": string; "steerable": boolean; [key: string]: unknown; };
export type Read = {
  /** The setting's dotted key. */
  "key": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type Reason = ({ "kind": "rule"; "rule": string; [key: string]: unknown; } | { "kind": "mode"; "mode": string; [key: string]: unknown; } | { "hook": string; "kind": "hook"; [key: string]: unknown; } | { "detail": string; "kind": "safety"; [key: string]: unknown; } | { "kind": "readOnly"; [key: string]: unknown; } | { "detail": string; "kind": "confirm"; [key: string]: unknown; } | { "kind": "default"; [key: string]: unknown; });
export type ReasoningProps = { "status": ItemStatus; "text": string; [key: string]: unknown; };
/**
 * A fixed rectangle: `width` by `height`, offset by `x` and `y` from
 * `anchor`, a corner or the `center` (`topLeft` unless given).
 */
export type Rect = { "anchor"?: Anchor; "height": Length; "width": Length; "x"?: Length; "y"?: Length; [key: string]: unknown; };
export type RegisterCommand = { "callback": string; "session": SessionId; "spec": CommandSpec; [key: string]: unknown; };
export type RegisterTool = { "callback": string; "session": SessionId; "spec": ToolSpec; [key: string]: unknown; };
export type RenderRequest = {
  /** The site asked for. */
  "component": Component;
  /** The site's facts; their shape is the component's props. */
  "props": unknown;
  /** Which instance of the site; for `Pane`, the id given to `$.ui.open`. */
  "requestId": string;
  /** The asking surface. */
  "surface": SurfaceName;
  /** The cells the surface offers the drawing. */
  "viewport": Viewport;
  [key: string]: unknown;
};
export type ResolvedBy = ({ "kind": "client"; "name": string; "surface": string; [key: string]: unknown; } | { "kind": "plugin"; "name": string; "surface": string; [key: string]: unknown; } | { "kind": "kernel"; [key: string]: unknown; } | { "kind": "policy"; [key: string]: unknown; });
export type ResultValue = { "result": SubmitResult; [key: string]: unknown; };
export type ResultValue10 = { "result": Catalog; [key: string]: unknown; };
export type ResultValue11 = { "result": null; [key: string]: unknown; };
export type ResultValue12 = { "result": Input; [key: string]: unknown; };
export type ResultValue13 = { "result": Append; [key: string]: unknown; };
export type ResultValue14 = { "result": CommandReply; [key: string]: unknown; };
export type ResultValue15 = { "result"?: (CommandSpec | null); [key: string]: unknown; };
export type ResultValue16 = { "result": ConfigSet; [key: string]: unknown; };
export type ResultValue17 = { "result": ConfigView; [key: string]: unknown; };
export type ResultValue18 = { "result": unknown; [key: string]: unknown; };
export type ResultValue2 = { "result": CheckResult; [key: string]: unknown; };
export type ResultValue3 = { "result": ToolCallAnswer; [key: string]: unknown; };
export type ResultValue4 = { "result": _Result_5f_of_5f_Array_5f_of_5f_ModelEvent_5f_or_5f_ProviderError; [key: string]: unknown; };
export type ResultValue5 = { "result": ModelRequest; [key: string]: unknown; };
export type ResultValue6 = { "result": _Result_5f_of_5f_Compaction_5f_or_5f_CompactError; [key: string]: unknown; };
export type ResultValue7 = { "result": StopResult; [key: string]: unknown; };
export type ResultValue8 = { "result"?: (string | null); [key: string]: unknown; };
export type ResultValue9 = { "result"?: (ToolSpec | null); [key: string]: unknown; };
export type _Result_5f_of_5f_Array_5f_of_5f_ModelEvent_5f_or_5f_ProviderError = ({ "Ok": Array<ModelEvent>; [key: string]: unknown; } | { "Err": ProviderError; [key: string]: unknown; });
export type _Result_5f_of_5f_Compaction_5f_or_5f_CompactError = ({ "Ok": Compaction; [key: string]: unknown; } | { "Err": CompactError; [key: string]: unknown; });
export type RewindProps = { "dropped": number; "toTurn": TurnId; [key: string]: unknown; };
export type Role = ("user" | "assistant");
export type Run = {
  /** Its arguments. */
  "args": unknown;
  /** The command's name, without the slash. */
  "name": string;
  "session": SessionId;
  [key: string]: unknown;
};
export type SelectOption = {
  /** The text shown. */
  "label": string;
  /** The value chosen. */
  "value": string;
  [key: string]: unknown;
};
export type Send = {
  /** Whether it wakes the receiver or waits for its next turn. */
  "delivery": Delivery;
  /** What is sent. */
  "input": Input;
  /** The receiving session. */
  "to": SessionId;
  [key: string]: unknown;
};
/**
 * Position of a frame in a session's journal. Durable frames are gapless;
 * clients only require monotonic order.
 */
export type Seq = number;
/**
 * One call on one service, the same shape whichever way it goes: the key
 * that names the service, the method, and whatever that method takes.
 *
 * Neither side reads `params`: it is the service's own contract, written in
 * the schema the declaration carries, and the bridge is a pipe for it.
 */
export type ServiceCallParams = { "key": string; "method": string; "params"?: unknown; [key: string]: unknown; };
/**
 * What the service answered. A service with nothing to say answers `null`,
 * which is an answer; a service that could not answer fails the call.
 */
export type ServiceCallResult = { "result"?: unknown; [key: string]: unknown; };
export type ServiceMethod = ({ "input": unknown; "result": unknown; "unsafe"?: boolean; [key: string]: unknown; } | unknown);
/**
 * What one service speaks: a name for every method it answers, and the JSON
 * Schema of what that method takes.
 *
 * The schema is for whoever writes the caller — nothing here validates
 * against it, as nothing validates a manifest's `config` schema — and the
 * names are the host's: a method the declaration never named is refused
 * before it crosses, with the set that is named (ADR-0031 §5). A service
 * that names no method speaks none.
 */
export type ServiceSpec = { "methods"?: { [key: string]: unknown; }; "noun"?: (string | null); [key: string]: unknown; };
export type Session = {
  /** The session this concerns; `$` methods default it to the current one. */
  "session": SessionId;
  [key: string]: unknown;
};
/** One persisted context: one journal, one actor. */
export type SessionId = string;
export type SessionModeProps = { "modes": Array<string>; [key: string]: unknown; };
export type SessionStatusProps = { "context"?: (ContextUsage | null); "cwd": string; "effort"?: (Effort | null); "model"?: (string | null); "modelScope"?: (ChoiceScope | null); "provider"?: (string | null); "title"?: (string | null); "usage": Usage; [key: string]: unknown; };
export type SessionSummary = {
  /** An interaction is open; stamped only by the session actor. */
  "asking"?: boolean;
  /**
   * A turn is running. A summary travels without the turn it describes —
   * a host listing carries no `SessionState` — so the kernel stamps this
   * on every summary it publishes and nothing else writes it. A client
   * that holds the state derives the same fact with `SessionState::busy`.
   */
  "busy"?: boolean;
  "createdAt": string;
  "cwd": string;
  "driver"?: Driver;
  "effort"?: (Effort | null);
  "firstAsk"?: (string | null);
  "forkedFrom"?: (ForkLink | null);
  "id": SessionId;
  "key"?: (string | null);
  /**
   * How many messages have been said in this session, by
   * [`Event::completes_a_message`]. `None` is "never counted" — a summary
   * written before the field existed — and must be shown as nothing rather
   * than as a `0` it does not mean.
   */
  "messages"?: (number | null);
  "model"?: (string | null);
  "modelScope"?: (ChoiceScope | null);
  "parent"?: (ParentLink | null);
  "provider"?: (string | null);
  /**
   * What the session was opened with and a resume must give it back:
   * the prompt appended to the kernel's own, and the tool set it is held to.
   */
  "systemExtra"?: (string | null);
  "title"?: (string | null);
  "tools"?: (Array<string> | null);
  "updatedAt": string;
  "usage"?: Usage;
  [key: string]: unknown;
};
export type SessionsProps = {
  "action": Action;
  "cwd": string;
  "limit": number;
  "now": string;
  "query": string;
  "scope": SessionsScope;
  /** The viewed session: it owns the opening and is the row marked current. */
  "session": SessionId;
  /** One extra summary is the sentinel for the next page. */
  "sessions": Array<SessionSummary>;
  [key: string]: unknown;
};
export type SessionsScope = ("here" | "all");
export type Setting = {
  /** The setting's dotted key. */
  "key": string;
  "session": SessionId;
  /** Its new value. */
  "value": unknown;
  [key: string]: unknown;
};
export type ShellRunProps = { "command": string; "cwd": string; "exit"?: (number | null); "output": string; "status": ItemStatus; [key: string]: unknown; };
export type Shown = ({ "request": PullRequest; [key: string]: unknown; } | View);
export type SignInProps = { "flow": LoginFlow; "interactionId": InteractionId; "provider": string; [key: string]: unknown; };
export type SiteHints = { "composer"?: (ComposerProps | null); "pairs"?: Array<KeyPair>; [key: string]: unknown; };
export type SiteScroll = { "bodyRows": number; "offset": number; [key: string]: unknown; };
export type SiteView = { "agentId"?: (string | null); [key: string]: unknown; };
export type Slot = ({ "branch"?: (string | null); "cwd": string; "kind": "location"; [key: string]: unknown; } | { "kind": "status"; "text": string; "tone": Tone; "until"?: (string | null); [key: string]: unknown; } | { "kind": "activity"; "verb"?: (string | null); "view": Shown; [key: string]: unknown; } | { "kind": "rail"; "title": string; "view": Shown; [key: string]: unknown; } | { "kind": "panel"; "title": string; "view": Shown; [key: string]: unknown; } | { "asked": boolean; "closeOnEscape": boolean; "focus": boolean; "hints"?: (SiteHints | null); "holdToasts": boolean; "id": string; "kind": "pane"; "place"?: Array<PanePlace>; "size"?: PaneSize; "title": string; "view": Shown; [key: string]: unknown; } | { "alert": boolean; "kind": "toast"; "text": string; "until": string; [key: string]: unknown; } | {
  /** The mode's value: the argument of the item that chooses it. */
  "current": string;
  "kind": "mode";
  /** The first label is the resting mode, which a surface may leave unsaid. */
  "modes": Array<ActionItem>;
  [key: string]: unknown;
} | { "actions": Array<ActionItem>; "badge"?: (string | null); "body"?: (View | null); "caption"?: (string | null); "item": ItemId; "kind": "row"; "mark"?: (string | null); "title"?: (string | null); [key: string]: unknown; });
/** A size the surface fits between `min` and `max`, preferring `ideal`. */
export type Span = {
  /** What it takes when there is room. */
  "ideal": Length;
  /** The largest it may be. */
  "max": Length;
  /** The smallest it may be. */
  "min": Length;
  [key: string]: unknown;
};
export type SpinnerProps = { "message"?: (string | null); "mode": string; "suffix": string; "word": string; [key: string]: unknown; };
export type StateGet = { "key": string; "scope": string; [key: string]: unknown; };
export type StateSet = { "ifVersion"?: (number | null); "key": string; "scope": string; "value": unknown; [key: string]: unknown; };
export type StateValue = { "value": unknown; "version": number; [key: string]: unknown; };
/** Optional facts supplied by the asking surface, without transcript bodies. */
export type StatusProps = { "config"?: ConfigView; "context"?: (ContextUsage | null); "selected"?: (string | null); "summary"?: SessionSummary; [key: string]: unknown; };
export type StopResult = ("complete" | { "continue": { "origin": string; "reason": string; [key: string]: unknown; }; });
export type Submit = {
  /** What to submit, as a person would type it. */
  "input": Input;
  "session": SessionId;
  [key: string]: unknown;
};
export type SubmitResult = ({ "accepted": Input; } | { "rejected": KernelError; } | { "redirect": { "input": Input; "session": SessionId; [key: string]: unknown; }; });
export type SurfaceName = ("terminal" | "desktop" | "text" | "chat");
/** One cacheable segment of the system prompt. */
export type SystemBlock = { "cache"?: boolean; "text": string; [key: string]: unknown; };
/** A text child preserves the style of its nested Text node. */
export type TextChild = (string | View);
export type Tier = ("tighten" | "rewrite" | "full");
export type Timer = {
  "callback": string;
  /** Milliseconds. */
  "ms": number;
  "session": SessionId;
  [key: string]: unknown;
};
export type ToastProps = { "alert": boolean; "text": string; "until": string; [key: string]: unknown; };
/** The one styling hook a plugin has; the surface owns the colour. */
export type Tone = (("neutral" | "good" | "bad") | "attention");
/** A call the model asked for, before or after the gate. */
export type ToolCall = { "callId": string; "input": unknown; "name": string; [key: string]: unknown; };
/** A denial is an answer, never an invalid output that falls through to execution. */
export type ToolCallAnswer = ({ "deny": string; [key: string]: unknown; } | ToolOutput);
/**
 * One call, named as the plugin named it — the `plugin__<name>__` prefix is
 * the model's and the permission grammar's, never the process's.
 */
export type ToolCallParams = { "callId": string; "cwd": string; "input": unknown; "name": string; "session": SessionId; "turn": TurnId; [key: string]: unknown; };
export type ToolCallResult = { "output": ToolOutput; [key: string]: unknown; };
/**
 * Kernel → plugin: this call's turn was interrupted, and nobody is waiting
 * for the answer any more. The host has already let the call go, so a process
 * that ignores this is not holding anything up — it is only doing work whose
 * result will be thrown away.
 */
export type ToolCancelParams = { "callId": string; [key: string]: unknown; };
/**
 * A tool call as a hook sees it: the tool's own arguments sit beside
 * `tool` and `tool_use_id`, not under an `input` field.
 */
export type ToolEvent = {
  /** The tool's name, such as `Bash` or `Edit`. */
  "tool": string;
  /** The model's id for this call. */
  "tool_use_id": string;
  [key: string]: unknown;
};
export type ToolGroupProps = { "calls": Array<ToolUseProps>; "isActive": boolean; "isExpanded": boolean; [key: string]: unknown; };
/**
 * What a tool returned: the parts the model sees, plus what a person sees
 * instead when the tool has something better than the text (ADR-0013 §2).
 */
export type ToolOutput = { "display"?: (View | null); "isError"?: boolean; "parts": Array<ContentPart>; [key: string]: unknown; };
/** Plugin → kernel, while a call runs: the whole of the live output line. */
export type ToolProgressParams = { "callId": string; "tail": string; [key: string]: unknown; };
export type ToolProgressProps = { "hint": string; "kind": string; "tool_use_id": string; [key: string]: unknown; };
export type ToolResultProps = { "isErrored": boolean; "output": unknown; "tool": string; "tool_use_id": string; [key: string]: unknown; };
/** A tool as the model sees it. */
export type ToolSpec = {
  "description": string;
  "inputSchema": unknown;
  /**
   * What a catalogue may show beside the tool (an MCP tool's `server`);
   * never sent to a model.
   */
  "meta"?: { [key: string]: unknown; };
  "name": string;
  [key: string]: unknown;
};
export type ToolUseProps = { "input": unknown; "isErrored": boolean; "isInterrupted": boolean; "isRunning": boolean; "output"?: unknown; "tool": string; "tool_use_id": string; [key: string]: unknown; };
export type ToolsChanged = { "tools": Array<ToolSpec>; [key: string]: unknown; };
export type TreeNode = { "badge"?: (string | null); "children"?: Array<TreeNode>; "label": string; "tone"?: Tone; [key: string]: unknown; };
export type Turn = {
  /** The assistant's final text. */
  "answer": string;
  /** How it ended. */
  "outcome": TurnStatus;
  /** The turn that ended. */
  "turn": TurnId;
  /** Tokens the turn used. */
  "usage": Usage;
  [key: string]: unknown;
};
/**
 * One turn as a row: its headline only. The whole prompt stays in the
 * transcript, where `asked` reads it.
 */
export type Turn2 = { "first": number; "headline"?: (string | null); "id": TurnId; "last": number; [key: string]: unknown; };
export type TurnDurationProps = { "durationMs": number; "word": string; [key: string]: unknown; };
/** One run of the turn loop, closed exactly once. */
export type TurnId = string;
export type TurnOrigin = ("submit" | "queue" | "peer" | "auto");
export type TurnProgressProps = { "elapsedMs": number; "interruptible": boolean; "stopping": boolean; "tokensOut": number; [key: string]: unknown; };
export type TurnStatus = ({ "kind": "completed"; [key: string]: unknown; } | { "error": KernelError; "kind": "failed"; [key: string]: unknown; } | { "kind": "interrupted"; "reason": InterruptReason; [key: string]: unknown; });
export type UnifiedFinish = ("stop" | "length" | "toolCalls" | "contentFilter" | "error" | "other");
/**
 * Token counts as the provider reports them, the three input counts kept
 * apart: `input_tokens` is what the model read fresh, the cache counts are
 * what it read from or wrote to a cached prefix. The whole input side is
 * `input_total()`.
 */
export type Usage = { "cacheReadTokens"?: number; "cacheWriteTokens"?: number; "inputTokens": number; "outputTokens": number; "reasoningTokens"?: number; [key: string]: unknown; };
export type UsageView = {
  /** The current context window reading, once known. */
  "context"?: (ContextUsage | null);
  /** Tokens used by the last turn. */
  "lastTurn"?: (Usage | null);
  /** Tokens used by the whole session. */
  "session": Usage;
  [key: string]: unknown;
};
export type UserMessageProps = { "isExpanded": boolean; "origin": Origin; "text": string; [key: string]: unknown; };
export type View = ({
  /** Draw the text bold. */
  "bold"?: (boolean | null);
  /** Strings and nested `Text` nodes, drawn in order. */
  "children": Array<TextChild>;
  /** A colour name or `#rrggbb`; the surface may adjust it to its theme. */
  "color"?: (string | null);
  /** Draw the text dimmed. */
  "dimColor"?: (boolean | null);
  /** Swap the foreground and background colours. */
  "inverse"?: (boolean | null);
  /** Draw the text in italics. */
  "italic"?: (boolean | null);
  "kind": "Text";
  /** Strike the text through. */
  "strikethrough"?: (boolean | null);
  /** Underline the text. */
  "underline"?: (boolean | null);
  /** `wrap` breaks long lines at the width; `truncate` cuts them. */
  "wrap"?: (string | null);
  [key: string]: unknown;
} | {
  /** Draw it dimmed. */
  "dimColor"?: boolean;
  "kind": "Markdown";
  /** The Markdown source. */
  "text": string;
  [key: string]: unknown;
} | {
  "kind": "Code";
  /** The language name for highlighting, such as `rust`. */
  "language"?: (string | null);
  /** The code itself. */
  "source": string;
  [key: string]: unknown;
} | {
  "kind": "diff";
  /** The diff in unified format. */
  "unified": string;
  [key: string]: unknown;
} | {
  /** One string per item. */
  "items": Array<string>;
  "kind": "list";
  [key: string]: unknown;
} | {
  /** The column headings. */
  "headers": Array<string>;
  "kind": "table";
  /** One array of cells per row, in column order. */
  "rows": Array<Array<string>>;
  [key: string]: unknown;
} | {
  "kind": "keyValue";
  /** `[label, value]` pairs, in order. */
  "rows": Array<[string, string, ...Array<unknown>]>;
  [key: string]: unknown;
} | {
  "kind": "progress";
  /** Text drawn beside the bar. */
  "label"?: (string | null);
  /** The whole; without it the surface shows a count. */
  "total"?: (number | null);
  /** How much is done. */
  "value": number;
  [key: string]: unknown;
} | {
  "kind": "badge";
  /** The label. */
  "text": string;
  /** `neutral`, `good`, `bad` or `attention`; the surface picks the colour. */
  "tone"?: Tone;
  [key: string]: unknown;
} | {
  "kind": "tree";
  /** The top-level nodes. */
  "nodes": Array<TreeNode>;
  [key: string]: unknown;
} | {
  /** Cross-axis alignment: `flex-start`, `center`, `flex-end` or `stretch`. */
  "alignItems"?: (Align | null);
  /** The border colour. */
  "borderColor"?: (string | null);
  /** Any value but `none` draws a border. */
  "borderStyle"?: (string | null);
  /** The views inside, in order. */
  "children": Array<View>;
  /** `none` hides the box and its controls. */
  "display"?: (string | null);
  /** `row` or `column` (the default). */
  "flexDirection"?: (Direction | null);
  /** How much of the spare room this box takes; `0` takes none. */
  "flexGrow"?: (number | null);
  /** `wrap` lets children flow onto another line. */
  "flexWrap"?: (Wrap | null);
  /** Cells between children. */
  "gap"?: (number | null);
  /** The height in rows. */
  "height"?: (number | null);
  /** The focused site declares chrome separately from its body. */
  "hints"?: (SiteHints | null);
  /** Main-axis distribution, such as `flex-start` or `space-between`. */
  "justifyContent"?: (Justify | null);
  /** A stable identity among siblings. */
  "key"?: (string | null);
  "kind": "Box";
  /** Cells outside the box on every side. */
  "margin"?: (number | null);
  /** Cells outside the box on the left and right. */
  "marginX"?: (number | null);
  /** Rows outside the box above and below. */
  "marginY"?: (number | null);
  /** A handler for a key pressed while this box's pane has focus; return `{handled: false}` to give the key back. */
  "onKey"?: (Action | null);
  /** Cells inside the box on every side. */
  "padding"?: (number | null);
  /** Cells inside the box on the left and right. */
  "paddingX"?: (number | null);
  /** Rows inside the box above and below. */
  "paddingY"?: (number | null);
  /**
   * A question owner's editing state, on the root of a drawn question;
   * the choices' labels and the answer stay the host's.
   */
  "question"?: (QuestionFocus | null);
  /** The width in cells. */
  "width"?: (number | null);
  [key: string]: unknown;
} | {
  /** The view inside. */
  "child": View;
  "kind": "panel";
  /** The heading. */
  "title": string;
  [key: string]: unknown;
} | {
  /** The buttons, in order. */
  "items": Array<ActionItem>;
  "kind": "actions";
  [key: string]: unknown;
} | {
  /** The button's text, when `label` is not given. */
  "children"?: Array<TextChild>;
  /** Draw the button dimmed. */
  "dimColor"?: (boolean | null);
  /** One letter: pressed with Alt in a band, or alone in a focused pane, it presses the button. */
  "hotkey"?: (string | null);
  /** A stable identity among siblings. */
  "key"?: (string | null);
  "kind": "Button";
  /** The button's text. */
  "label"?: (string | null);
  /** The handler run when the button is pressed: a mod passes a function. */
  "onPress"?: (Action | null);
  /** `primary` draws it bold, `danger` in the alarm colour. */
  "variant"?: (string | null);
  [key: string]: unknown;
} | {
  /** A stable identity among siblings. */
  "key"?: (string | null);
  "kind": "Input";
  /** Text before the field. */
  "label"?: (string | null);
  /** The handler run as the text changes: a mod passes a function. */
  "onInput"?: (Action | null);
  /** The handler run when the person presses Enter: a mod passes a function. */
  "onSubmit"?: (Action | null);
  /** Text shown while the field is empty. */
  "placeholder"?: (string | null);
  /** The text in the field. */
  "value"?: (string | null);
  [key: string]: unknown;
} | {
  /** A stable identity among siblings. */
  "key"?: (string | null);
  "kind": "Select";
  /** Text before the choice. */
  "label"?: (string | null);
  /** The handler run with the chosen value: a mod passes a function. */
  "onSelect"?: (Action | null);
  /** The choices, each `{label, value}`. */
  "options"?: (Array<SelectOption> | null);
  /** The chosen option's `value`. */
  "value"?: (string | null);
  [key: string]: unknown;
} | {
  /** The URL. */
  "href": string;
  "kind": "Link";
  /** The text shown; the URL when absent. */
  "label"?: (string | null);
  [key: string]: unknown;
} | {
  /** Text shown where the image cannot be. */
  "alt": string;
  /** The width in terminal columns. */
  "columns"?: (number | null);
  "kind": "Image";
  /** The height in terminal rows. */
  "rows"?: (number | null);
  /** A path or a `data:` URL. */
  "source": string;
  [key: string]: unknown;
} | {
  /** The cells, row by row. */
  "cells": Array<string>;
  /** The width in cells. */
  "columns": number;
  "kind": "Raster";
  /** The height in cells. */
  "rows": number;
  [key: string]: unknown;
} | {
  /** Text shown where the drawing cannot be. */
  "alt": string;
  /** The height in pixels. */
  "height"?: (number | null);
  "kind": "Svg";
  /** The SVG markup. */
  "source": string;
  /** The width in pixels. */
  "width"?: (number | null);
  [key: string]: unknown;
} | {
  /** The component bingo draws, such as `ToolUse`. */
  "component": Component;
  "kind": "Engine";
  /** That component's props, as its `ui.render` event carried them. */
  "props": unknown;
  [key: string]: unknown;
} | {
  /** The element's own name. */
  "customKind": string;
  /** Whatever the element needs. */
  "data": unknown;
  /** The plain text drawn where the element is unknown. */
  "fold": string;
  "kind": "custom";
  [key: string]: unknown;
});
export type Viewport = {
  /** Width in cells. */
  "columns": number;
  /** Where a pane sits, when the site is a pane. */
  "place"?: (PanePlace | null);
  /** Height in cells. */
  "rows": number;
  [key: string]: unknown;
};
/** How a producer intends an utterance to be drawn. */
export type Voice = ("said" | "note" | "quiet" | "aside");
export type WelcomeProps = { "effort"?: (Effort | null); "modes": Array<string>; "resume"?: (SessionSummary | null); "session": SessionSummary; "update"?: (string | null); [key: string]: unknown; };
export type Wrap = ("wrap" | "nowrap");
