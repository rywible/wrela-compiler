// The page's WebRTC for the program's relays (relay.ts), on the main thread: a worker can't make
// a WebRTC connection, so the render worker asks here (messages.ts `rtc`) and is handed each
// negotiated data channel.

import { CONNECT_MS, RELAY_CHANNEL, type RtcAsk, STUN } from "./relay.ts";

/** The main thread's WebRTC connections, one a relay. */
export class RtcHost {
  readonly #connections = new Map<number, { pc: RTCPeerConnection; kept: RTCDataChannel[] }>();

  /** Carries out what the worker asked: an answer's SDP, a channel (to be handed over), or
   * nothing. */
  async ask(a: RtcAsk): Promise<string | RTCDataChannel | null> {
    switch (a.op) {
      case "answer": {
        const c = this.#open(a.relay);
        await c.pc.setRemoteDescription({ type: "offer", sdp: a.sdp });
        await c.pc.setLocalDescription(await c.pc.createAnswer());
        await gathered(c.pc);
        return c.pc.localDescription!.sdp;
      }
      case "connected":
        return this.#connected(a.relay).then(() => null);
      case "channel": {
        const c = this.#connections.get(a.relay);
        if (c === undefined) throw new Error(`relay ${a.relay} has no connection`);
        return c.pc.createDataChannel(RELAY_CHANNEL, { negotiated: true, id: a.id, ordered: false, maxRetransmits: 0 });
      }
      case "close": {
        const c = this.#connections.get(a.relay);
        this.#connections.delete(a.relay);
        if (c !== undefined) {
          for (const k of c.kept) k.close();
          c.pc.close();
        }
        return null;
      }
    }
  }

  #open(relay: number) {
    let c = this.#connections.get(relay);
    if (c === undefined) {
      const pc = new RTCPeerConnection({ iceServers: [{ urls: STUN }], bundlePolicy: "max-bundle" });
      c = { pc, kept: [] };
      const kept = c.kept;
      // The SFU opens its own channel for the transport; it's kept, to close with the rest.
      pc.ondatachannel = (e: RTCDataChannelEvent) => kept.push(e.channel);
      this.#connections.set(relay, c);
    }
    return c;
  }

  #connected(relay: number): Promise<void> {
    const c = this.#connections.get(relay);
    if (c === undefined) return Promise.reject(new Error(`relay ${relay} has no connection`));
    const pc = c.pc;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("the relay's WebRTC didn't connect in time")), CONNECT_MS);
      const check = () => {
        if (pc.connectionState === "connected") {
          clearTimeout(timer);
          resolve();
        } else if (pc.connectionState === "failed" || pc.connectionState === "closed") {
          clearTimeout(timer);
          reject(new Error(`the relay's WebRTC ${pc.connectionState}`));
        }
      };
      pc.addEventListener("connectionstatechange", check);
      check();
    });
  }
}

/** Settles once `pc` has gathered its candidates (or after a few seconds, with what it has). */
function gathered(pc: RTCPeerConnection): Promise<void> {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      if (pc.iceGatheringState === "complete") resolve();
    };
    pc.addEventListener("icegatheringstatechange", done);
    setTimeout(resolve, 3000);
  });
}

