// SPDX-License-Identifier: Apache-2.0
// Mutual authentication in `session.hello` (PROTOCOL.md §1), the TypeScript
// side of `crates/proto/src/auth.rs`. The client sends a fresh nonce; the
// daemon answers with HMAC-SHA256(key = token, "whoosh-hello-proof-v1\n" + nonce).
// A process squatting the port of a stale runtime file cannot compute it, so
// the client sends nothing else to it.

export const PROOF_CONTEXT = "whoosh-hello-proof-v1\n";
export const NONCE_BYTES = 16;

function subtle(): SubtleCrypto {
  const s = globalThis.crypto?.subtle;
  if (!s) throw new Error("WebCrypto is not available: cannot authenticate the daemon");
  return s;
}

function toHex(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}

function fromHex(s: string): Uint8Array<ArrayBuffer> | undefined {
  if (s.length % 2 !== 0 || !/^[0-9a-f]*$/.test(s)) return undefined;
  const out = new Uint8Array(new ArrayBuffer(s.length / 2));
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** A fresh nonce: 16 random bytes, lower-case hex. */
export function newNonce(): string {
  const bytes = new Uint8Array(NONCE_BYTES);
  globalThis.crypto.getRandomValues(bytes);
  return toHex(bytes);
}

/** Whether `proof` answers `nonce` for `token`. WebCrypto's verify compares in constant time. */
export async function verifyHelloProof(token: string, nonce: string, proof: unknown): Promise<boolean> {
  if (typeof proof !== "string") return false;
  const sig = fromHex(proof);
  if (!sig || sig.length !== 32) return false;
  const enc = new TextEncoder();
  const key = await subtle().importKey("raw", enc.encode(token), { name: "HMAC", hash: "SHA-256" }, false, ["verify"]);
  return subtle().verify("HMAC", key, sig, enc.encode(PROOF_CONTEXT + nonce));
}
