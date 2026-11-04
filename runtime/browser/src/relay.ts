// A room's relay (language.md §19.3, `std::net::relay`): a lossy, unordered channel between a
// room's tagged peers that doesn't pass through the room. On the Cloudflare host it goes through
// Realtime SFU: the room's object holds the SFU's credentials and speaks for each peer, over text
// messages on the peer's own WebSocket to the room (`Control`); the page's WebRTC connects to the
// SFU alone, so no peer learns another's address. A WebRTC connection lives on the main thread
// (a worker has none), which hands each negotiated data channel to the render worker (`Rtc`).

/** The most bytes a relay message carries (`std::net::MAX_RELAY`). */
export const MAX_RELAY = 1024;
/** The bytes a relay's channel may hold waiting to be sent: past them, a send is dropped, as the
 * network would. */
export const RELAY_PENDING = 64 * 1024;
/** The name of every peer's channel on the SFU. */
export const RELAY_CHANNEL = "relay";
/** The STUN server the page's WebRTC asks (Cloudflare's, as the SFU's docs give). */
export const STUN = "stun:stun.cloudflare.com:3478";

/** A text message about a relay, on its peer's WebSocket to the room. From the peer: `join`,
 * the page's `answer` to the SFU's offer, `ready` once its WebRTC connected, `leave`. From the
 * room: the SFU's `offer`, the channel it `publish`es on, a `peer`'s channel it hears on, a peer
 * `gone`, and `closed` (refused, or ended), which ends it. */
export type Control =
  | { relay: "join" }
  | { relay: "answer"; sdp: string }
  | { relay: "ready" }
  | { relay: "leave" }
  | { relay: "offer"; sdp: string }
  | { relay: "publish"; id: number }
  | { relay: "peer"; peer: number; id: number }
  | { relay: "gone"; peer: number }
  | { relay: "closed"; why: string };

const channelId = (x: unknown) => Number.isInteger(x) && (x as number) >= 0 && (x as number) < 65535;
const tagOf = (x: unknown) => Number.isInteger(x) && (x as number) > 0 && (x as number) < 2 ** 32;

/** A control message as `text` holds it, or null if it isn't one. */
export function parseControl(text: string): Control | null {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null) return null;
  const m = v as Record<string, unknown>;
  switch (m["relay"]) {
    case "join":
    case "ready":
    case "leave":
      return { relay: m["relay"] };
    case "answer":
    case "offer":
      return typeof m["sdp"] === "string" ? { relay: m["relay"], sdp: m["sdp"] } : null;
    case "publish":
      return channelId(m["id"]) ? { relay: "publish", id: m["id"] as number } : null;
    case "peer":
      return channelId(m["id"]) && tagOf(m["peer"]) ? { relay: "peer", peer: m["peer"] as number, id: m["id"] as number } : null;
    case "gone":
      return tagOf(m["peer"]) ? { relay: "gone", peer: m["peer"] as number } : null;
    case "closed":
      return typeof m["why"] === "string" ? { relay: "closed", why: m["why"] } : null;
    default:
      return null;
  }
}

/** A data channel, as a relay uses it (RTCDataChannel's). */
export interface Channel {
  readonly readyState: string;
  readonly bufferedAmount: number;
  binaryType: string;
  onopen: ((e: unknown) => void) | null;
  onmessage: ((e: { data: unknown }) => void) | null;
  onclose: ((e: unknown) => void) | null;
  send(bytes: Uint8Array<ArrayBuffer>): void;
  close(): void;
}

/** The page's WebRTC, as the render worker reaches it: each relay `relay`'s connection to the
 * SFU, made on its first `answer`. */
export interface Rtc {
  /** The page's answer to the SFU's `offer`, once its candidates are gathered. */
  answer(relay: number, offer: string): Promise<string>;
  /** Settles once the connection is up; fails if it fails or doesn't come up in time. */
  connected(relay: number): Promise<void>;
  /** The negotiated channel `id` (unordered, never resent), handed to the worker. */
  channel(relay: number, id: number): Promise<Channel>;
  /** Closes the relay's connection, and its channels. */
  close(relay: number): void;
}

/** What the worker asks the main thread for, and what it answers (messages.ts's `rtc`). */
export type RtcAsk =
  | { op: "answer"; relay: number; sdp: string }
  | { op: "connected"; relay: number }
  | { op: "channel"; relay: number; id: number }
  | { op: "close"; relay: number };

/** How long a relay's WebRTC may take to connect. */
export const CONNECT_MS = 15_000;

/** A relay's message as the program reads it: the sender's tag (little-endian), then its bytes. */
export function fromPeer(peer: number, bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(4 + bytes.length);
  new DataView(out.buffer).setUint32(0, peer, true);
  out.set(bytes, 4);
  return out;
}
