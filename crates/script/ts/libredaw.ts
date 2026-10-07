// SPDX-License-Identifier: GPL-3.0-or-later
//
// LibreDAW scripting API, version 1 (SPEC 10). Control rate only: scripts
// never see audio. Import it as:
//
//   import { project, edit } from "libredaw";
//
// The host (LibreDAW) runs the script as `deno run` with no permissions
// except reading the script file. This module talks to the host over
// stdin/stdout, one JSON object per line, every object marked `"lr": 1`.
// `console.log` is redirected to stderr because stdout belongs to the
// protocol.

export const API_VERSION = 1;

/** Ticks per quarter note. A sixteenth-note step is 240 ticks. */
export const PPQ = 960;

// ---- Types (mirror the Rust `protocol` crate's JSON) ----------------------

export type Id = number;

export interface Mix {
  volume_db: number;
  pan: number;
  mute: boolean;
  solo: boolean;
}

export interface Note {
  id: Id;
  start: number;
  len: number;
  key: number;
  vel: number;
}

export interface Channel {
  id: Id;
  name: string;
  root_key: number;
  track: Id;
  mix: Mix;
  instrument: { kind: "synth" | "clap"; [field: string]: unknown };
}

export interface Pattern {
  id: Id;
  name: string;
  length_steps: number;
  step_ticks: number;
  notes: { channel: Id; notes: Note[] }[];
}

export interface Track {
  id: Id;
  name: string;
  mix: Mix;
  inserts: unknown[];
}

export interface Project {
  tempo_bpm: number;
  time_sig_num: number;
  metronome: { enabled: boolean; gain_db: number };
  channels: Channel[];
  patterns: Pattern[];
  tracks: Track[];
}

/** An `Edit` value as the Rust `protocol::edit::Edit` serializes it. */
export interface RawEdit {
  edit: string;
  [field: string]: unknown;
}

export interface Applied {
  /** Document revision after the batch. */
  revision: number;
  /** Ids created by the batch, in edit order. */
  created: Id[];
}

export type MixTarget = { channel: Id } | { track: Id };

/** The DAW refused a request. `code` is one of `stale`, `busy`,
 * `needs_user_approval`, `denied`, `not_allowed`, `edit`, `not_found`,
 * `too_large`, `bad_request`, `internal`. */
export class LibreDawError extends Error {
  readonly code: string;
  readonly detail: Record<string, unknown>;
  constructor(code: string, detail: Record<string, unknown>) {
    super(`LibreDAW error: ${code} ${JSON.stringify(detail)}`);
    this.name = "LibreDawError";
    this.code = code;
    this.detail = detail;
  }
}

// ---- Edit builder ---------------------------------------------------------

export class EditBuilder {
  /** @internal */
  readonly edits: RawEdit[] = [];

  /** Appends any `Edit` value (see the Rust `Edit` enum). */
  raw(edit: RawEdit): this {
    this.edits.push(edit);
    return this;
  }

  setTempo(bpm: number): this {
    return this.raw({ edit: "set_tempo", bpm });
  }

  /** One note. `start` and `len` are in ticks (960 per quarter note). */
  addNote(
    n: { pattern: Id; channel: Id; start: number; len: number; key: number; vel?: number },
  ): this {
    return this.addNotes(n.pattern, n.channel, [
      { start: n.start, len: n.len, key: n.key, vel: n.vel ?? 100 },
    ]);
  }

  addNotes(
    pattern: Id,
    channel: Id,
    notes: { start: number; len: number; key: number; vel?: number }[],
  ): this {
    return this.raw({
      edit: "add_notes",
      pattern,
      channel,
      notes: notes.map((n) => ({ start: n.start, len: n.len, key: n.key, vel: n.vel ?? 100 })),
    });
  }

  removeNotes(pattern: Id, notes: Id[]): this {
    return this.raw({ edit: "remove_notes", pattern, notes });
  }

  setStep(
    s: { pattern: Id; channel: Id; step: number; on: boolean; vel?: number },
  ): this {
    return this.raw({
      edit: "set_step",
      pattern: s.pattern,
      channel: s.channel,
      step: s.step,
      on: s.on,
      vel: s.vel ?? null,
    });
  }

  setVolume(target: MixTarget, db: number): this {
    return this.mix(target, { control: "volume_db", value: db });
  }

  /** -1 (left) to 1 (right). */
  setPan(target: MixTarget, pan: number): this {
    return this.mix(target, { control: "pan", value: pan });
  }

  setMute(target: MixTarget, on: boolean): this {
    return this.mix(target, { control: "mute", value: on });
  }

  setSolo(target: MixTarget, on: boolean): this {
    return this.mix(target, { control: "solo", value: on });
  }

  private mix(target: MixTarget, value: { control: string; value: number | boolean }): this {
    if ("channel" in target) {
      return this.raw({ edit: "set_channel_mix", channel: target.channel, value });
    }
    return this.raw({ edit: "set_track_mix", track: target.track, value });
  }
}

// ---- Host protocol --------------------------------------------------------

type Msg = Record<string, unknown> & { lr: 1 };

const encoder = new TextEncoder();
const decoder = new TextDecoder();

// Stdout is the protocol channel; keep stray output off it.
console.log = console.error;
console.info = console.error;
console.debug = console.error;

function send(msg: Record<string, unknown>): void {
  const bytes = encoder.encode(JSON.stringify({ lr: 1, ...msg }) + "\n");
  let off = 0;
  while (off < bytes.length) off += Deno.stdout.writeSync(bytes.subarray(off));
}

const stdin = Deno.stdin.readable.getReader();
let inbuf = "";

/** Reads one protocol line. Nothing is read while no request is pending, so
 * a script whose work is done exits instead of waiting on stdin. */
async function readMsg(): Promise<Msg> {
  for (;;) {
    const nl = inbuf.indexOf("\n");
    if (nl >= 0) {
      const line = inbuf.slice(0, nl);
      inbuf = inbuf.slice(nl + 1);
      try {
        const m = JSON.parse(line);
        if (m && m.lr === 1) return m as Msg;
      } catch {
        // Not ours; ignore.
      }
      continue;
    }
    const { value, done } = await stdin.read();
    if (done) throw new Error("LibreDAW closed the connection");
    inbuf += decoder.decode(value, { stream: true });
  }
}

type Pending = { resolve: (v: Record<string, unknown>) => void; reject: (e: Error) => void };
const pending = new Map<number, Pending>();
let nextId = 1;
let pumping = false;

async function pump(): Promise<void> {
  if (pumping) return;
  pumping = true;
  try {
    while (pending.size > 0) {
      const m = await readMsg();
      const id = m.id as number | undefined;
      const p = id === undefined ? undefined : pending.get(id);
      if (!p) continue;
      pending.delete(id!);
      if ("err" in m) {
        const e = m.err as Record<string, unknown>;
        p.reject(new LibreDawError(String(e.code ?? "internal"), e));
      } else {
        p.resolve((m.ok ?? {}) as Record<string, unknown>);
      }
    }
  } catch (e) {
    for (const p of pending.values()) p.reject(e as Error);
    pending.clear();
  } finally {
    pumping = false;
  }
}

function request(op: string, fields: Record<string, unknown>): Promise<Record<string, unknown>> {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    send({ id, op, ...fields });
    pump();
  });
}

// Handshake: the host sends `init` right after spawning us and kills the
// script if `ready` does not come back within 2 s. Top-level await keeps the
// script's own code from running before the handshake is done.
{
  const init = await readMsg();
  if (init.type !== "init") throw new Error("LibreDAW sent no init message");
  if (init.api !== API_VERSION) {
    throw new Error(`LibreDAW speaks scripting API ${init.api}, this module is ${API_VERSION}`);
  }
  send({ type: "ready", api: API_VERSION });
}

// ---- Public API -----------------------------------------------------------

let revision: number | null = null;

export const project = {
  /** A read-only snapshot of the document. */
  async get(): Promise<Project> {
    const r = await request("project.get", {});
    revision = r.revision as number;
    return r.project as Project;
  },
};

/**
 * Applies the edits built by `fn` as one undo group. The callback is
 * synchronous. Edits are checked against the revision this script last
 * saw (from `project.get()` or its own earlier edits); if the user changed
 * what they touch, this rejects with a `stale` error, so call
 * `project.get()` again and retry.
 */
export async function edit(fn: (e: EditBuilder) => void): Promise<Applied> {
  const b = new EditBuilder();
  const ret: unknown = fn(b);
  if (ret && typeof (ret as { then?: unknown }).then === "function") {
    throw new TypeError("edit() callback must be synchronous");
  }
  if (b.edits.length === 0) {
    return { revision: revision ?? 0, created: [] };
  }
  if (revision === null) await project.get();
  const r = await request("edit", { edits: b.edits, base_revision: revision });
  const applied = r as unknown as Applied;
  revision = applied.revision;
  return applied;
}
