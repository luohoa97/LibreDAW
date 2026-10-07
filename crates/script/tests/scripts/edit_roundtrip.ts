// SPDX-License-Identifier: GPL-3.0-or-later
import { edit, project } from "libredaw";

console.log("this goes to stderr, not the protocol");
const p = await project.get();
if (p.tempo_bpm !== 120) throw new Error("tempo " + p.tempo_bpm);

const applied = await edit((e) => {
  e.setTempo(140)
    .addNote({ pattern: 3, channel: 4, start: 0, len: 240, key: 36 })
    .setStep({ pattern: 3, channel: 4, step: 2, on: true })
    .removeNotes(3, [9])
    .setVolume({ track: 0 }, -3)
    .setPan({ channel: 4 }, 0.5)
    .setMute({ channel: 4 }, true)
    .setSolo({ track: 1 }, false);
});
if (applied.revision !== 8) throw new Error("revision " + applied.revision);
if (applied.created.join() !== "11,12") throw new Error("created " + applied.created);

// A second batch is checked against the revision the first one returned.
await edit((e) => e.setTempo(150));

// An empty batch is not sent at all.
const empty = await edit(() => {});
if (empty.created.length !== 0) throw new Error("empty");
