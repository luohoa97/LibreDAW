// SPDX-License-Identifier: GPL-3.0-or-later
import "libredaw";

async function denied(name: string, f: () => unknown | Promise<unknown>) {
  try {
    await f();
  } catch (e) {
    if ((e as Error).name === "NotCapable") return;
    if (name === "dynamic import" && (e as Error).message.includes("import access")) return;
    throw new Error(`${name}: wrong error ${(e as Error).name}: ${(e as Error).message}`);
  }
  throw new Error(`${name} was allowed`);
}

// Reading this one file is allowed.
const me = Deno.readTextFileSync(new URL(import.meta.url));
if (!me.includes("sandbox")) throw new Error("cannot read own source");

await denied("read other", () => Deno.readTextFileSync("/etc/hostname"));
await denied("read dir", () => Deno.readDirSync("/"));
await denied("write", () => Deno.writeTextFileSync("/tmp/libredaw-sandbox-test", "x"));
await denied("env", () => Deno.env.get("HOME"));
await denied("net", () => Deno.connect({ hostname: "127.0.0.1", port: 9 }));
await denied("unix", () => Deno.connect({ transport: "unix", path: "/run/user/0/x" }));
await denied("run", () => new Deno.Command("true").outputSync());
await denied("sys", () => Deno.hostname());
await denied("dynamic import", () => import("https://example.com/x.ts"));
