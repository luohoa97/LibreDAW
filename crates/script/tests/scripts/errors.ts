// SPDX-License-Identifier: GPL-3.0-or-later
import { edit, LibreDawError, project } from "libredaw";

await project.get();

// The DAW says stale: the script sees a typed error.
try {
  await edit((e) => e.setTempo(130));
  throw new Error("expected stale");
} catch (err) {
  if (!(err instanceof LibreDawError) || err.code !== "stale" || err.detail.current !== 9) {
    throw err;
  }
}

// An edit the Rust side cannot parse comes back as bad_request.
try {
  await edit((e) => e.raw({ edit: "explode" }));
  throw new Error("expected bad_request");
} catch (err) {
  if (!(err instanceof LibreDawError) || err.code !== "bad_request") throw err;
}

// Approval errors are surfaced as they are.
try {
  await edit((e) => e.setTempo(131));
  throw new Error("expected needs_user_approval");
} catch (err) {
  if (!(err instanceof LibreDawError) || err.code !== "needs_user_approval") throw err;
}

// An async callback is a programming error.
try {
  // deno-lint-ignore require-await
  await edit(async (e) => {
    e.setTempo(1);
  });
  throw new Error("expected TypeError");
} catch (err) {
  if (!(err instanceof TypeError)) throw err;
}
