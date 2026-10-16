// The accessibility overlay: the program's accessibility tree (`a11y/tree`) as elements over its
// canvas, for screen readers and the keyboard. The program says what's there (controls, and
// items to choose from); the page gives them roles, names and focus, and an action taken on one
// goes back to the program as an `Assist` event by its node's number, never as a click on the
// canvas: what a node is doesn't depend on what's drawn over it, or on the program's mode.
//
// Controls are buttons; items are the options of one listbox, which keeps the focus and moves
// the active option with the arrow keys (selecting it, as a single choice does). The elements
// are invisible but where the program draws them, and a focused one shows a ring, for a person
// who moves through them by keyboard (F6 from the canvas; Escape back).

import { assistEvent } from "./input.ts";
import { Reader } from "./wire.ts";

/** A node's role in the tree (its byte on the wire). */
export const ROLE_BUTTON = 0;
export const ROLE_ITEM = 1;
/** A node's flags. */
export const PRESSED = 1;
export const SELECTED = 2;
export const DISABLED = 4;

/** A node of the tree: its number, role, flags, rectangle (device pixels of the canvas) and name. */
export interface Node {
  id: number;
  role: number;
  flags: number;
  x: number;
  y: number;
  w: number;
  h: number;
  label: string;
}

/** A tree's bytes: its name, its items' list's name, then each node. */
export function readTree(body: Uint8Array): { name: string; items: string; nodes: Node[] } {
  const r = new Reader(body);
  const name = r.str();
  const items = r.str();
  const n = r.u32();
  if (n > 4096) throw new Error("an accessibility tree has at most 4096 nodes");
  const nodes: Node[] = [];
  for (let i = 0; i < n; i++) {
    const id = r.u32();
    const role = r.u8();
    const flags = r.u8();
    const [x, y, w, h] = [r.f32(), r.f32(), r.f32(), r.f32()];
    const label = r.str();
    if (role !== ROLE_BUTTON && role !== ROLE_ITEM) throw new Error(`no accessibility role ${role}`);
    nodes.push({ id, role, flags, x, y, w, h, label });
  }
  return { name, items, nodes };
}

const STYLE = `
.wrela-a11y { position: fixed; pointer-events: none; z-index: 1; }
.wrela-a11y button, .wrela-a11y [role=option] {
  position: absolute; margin: 0; padding: 0; border: 0; background: transparent;
  color: transparent; font-size: 1px; overflow: hidden; pointer-events: none; }
.wrela-a11y [role=listbox] { position: absolute; inset: 0; outline: none; }
.wrela-a11y button:focus-visible, .wrela-a11y [role=listbox]:focus .wrela-active {
  outline: 3px solid #4766ff; outline-offset: 2px; border-radius: 4px; }
.wrela-a11y .wrela-live { position: absolute; width: 1px; height: 1px; overflow: hidden;
  clip-path: inset(50%); white-space: nowrap; }
`;

/** The overlay's elements, made when the program first sends a tree or speaks. */
interface Parts {
  root: HTMLDivElement;
  controls: HTMLDivElement;
  list: HTMLDivElement;
  live: HTMLDivElement;
}

export class Overlay {
  #parts: Parts | null = null;
  #nodes = new Map<number, HTMLElement>();
  #order: number[] = [];
  #active: number | null = null;

  /** `write` puts an Assist event in the input ring; `place` gives the canvas's rectangle on the
   * page and its device pixels' size. */
  constructor(
    readonly canvas: HTMLCanvasElement,
    readonly write: (event: Uint8Array) => void,
    readonly place: () => { rect: DOMRect; width: number; height: number },
  ) {}

  /** The overlay's elements: a program that never sends a tree adds nothing to the page. */
  get #dom(): Parts {
    if (this.#parts) return this.#parts;
    const style = document.createElement("style");
    style.textContent = STYLE;
    document.head.append(style);
    const make = (cls: string) => Object.assign(document.createElement("div"), { className: cls });
    const parts = { root: make("wrela-a11y"), controls: make(""), list: make(""), live: make("wrela-live") };
    parts.controls.setAttribute("role", "group");
    parts.list.setAttribute("role", "listbox");
    parts.list.tabIndex = 0;
    parts.live.setAttribute("aria-live", "polite");
    parts.root.append(parts.controls, parts.list, parts.live);
    document.body.append(parts.root);
    parts.list.addEventListener("keydown", (e) => this.#key(e));
    parts.root.addEventListener("keydown", (e) => {
      if (e.key !== "Escape") return;
      this.canvas.focus();
      e.preventDefault();
    });
    this.#parts = parts;
    return parts;
  }

  /** The overlay's outermost element (made if it isn't yet). */
  get root(): HTMLDivElement {
    return this.#dom.root;
  }

  contains(e: Element | null): boolean {
    return e !== null && this.#parts !== null && this.#parts.root.contains(e);
  }

  /** Moves the keyboard's focus into the overlay: its first control, else its items. */
  enter(): boolean {
    if (this.#parts === null) return false;
    const first = this.#parts.controls.querySelector("button");
    if (first) first.focus();
    else if (this.#order.length > 0) this.#parts.list.focus();
    else return false;
    return true;
  }

  /** Says `text` to a screen reader, politely (after what it's saying). */
  announce(text: string): void {
    // A change is what's read: the same text twice is read twice.
    const live = this.#dom.live;
    live.textContent = "";
    setTimeout(() => (live.textContent = text), 50);
  }

  /** The tree is now `body`'s: elements are kept by node number, so focus stays where it was. */
  tree(body: Uint8Array): void {
    const { name, items, nodes } = readTree(body);
    const { root, controls, list } = this.#dom;
    controls.setAttribute("aria-label", name);
    list.setAttribute("aria-label", items);
    const { rect, width, height } = this.place();
    Object.assign(root.style, { left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px` });
    const sx = width > 0 ? rect.width / width : 1;
    const sy = height > 0 ? rect.height / height : 1;
    const seen = new Set<number>();
    for (const n of nodes) {
      seen.add(n.id);
      let el = this.#nodes.get(n.id);
      const role = n.role === ROLE_BUTTON ? "button" : "option";
      if (el && (el.tagName === "BUTTON") !== (role === "button")) {
        el.remove();
        el = undefined;
      }
      if (!el) {
        el = role === "button" ? document.createElement("button") : document.createElement("div");
        el.id = `wrela-node-${n.id}`;
        if (role === "button") {
          el.tabIndex = 0;
          el.addEventListener("click", () => this.write(assistEvent(n.id, 0)));
        } else {
          el.setAttribute("role", "option");
          el.addEventListener("click", () => this.#choose(n.id, 1));
        }
        this.#nodes.set(n.id, el);
      }
      (role === "button" ? controls : list).append(el);
      el.setAttribute("aria-label", n.label);
      el.textContent = n.label;
      Object.assign(el.style, { left: `${n.x * sx}px`, top: `${n.y * sy}px`, width: `${n.w * sx}px`, height: `${n.h * sy}px` });
      if (role === "button") {
        if (n.flags & PRESSED) el.setAttribute("aria-pressed", "true");
        else el.removeAttribute("aria-pressed");
        el.setAttribute("aria-disabled", String((n.flags & DISABLED) !== 0));
      } else {
        el.setAttribute("aria-selected", String((n.flags & SELECTED) !== 0));
      }
    }
    // Nodes that went: a focused one hands the focus to the items, or back to the canvas.
    for (const [id, el] of this.#nodes) {
      if (seen.has(id)) continue;
      const focused = el === document.activeElement;
      el.remove();
      this.#nodes.delete(id);
      if (focused) (list.childElementCount > 0 ? list : this.canvas).focus();
    }
    this.#order = nodes.filter((n) => n.role === ROLE_ITEM).map((n) => n.id);
    if (this.#active !== null && !this.#nodes.has(this.#active)) this.#active = this.#order[0] ?? null;
    this.#mark();
  }

  #mark(): void {
    for (const id of this.#order) this.#nodes.get(id)?.classList.toggle("wrela-active", id === this.#active);
    const list = this.#dom.list;
    if (this.#active === null) list.removeAttribute("aria-activedescendant");
    else list.setAttribute("aria-activedescendant", `wrela-node-${this.#active}`);
  }

  /** Makes item `id` the active option, and tells the program (`action`: 1 select, 0 activate). */
  #choose(id: number, action: number): void {
    this.#active = id;
    this.#mark();
    this.write(assistEvent(id, action));
  }

  /** The listbox's keys: the arrows, Home and End choose; Enter activates, Space selects. */
  #key(e: KeyboardEvent): void {
    if (this.#order.length === 0) return;
    const i = this.#active === null ? -1 : this.#order.indexOf(this.#active);
    const last = this.#order.length - 1;
    const to =
      e.key === "ArrowDown" || e.key === "ArrowRight" ? Math.min(i + 1, last)
      : e.key === "ArrowUp" || e.key === "ArrowLeft" ? Math.max(i - 1, 0)
      : e.key === "Home" ? 0
      : e.key === "End" ? last
      : null;
    if (to !== null) {
      this.#choose(this.#order[to]!, 1);
      e.preventDefault();
    } else if ((e.key === "Enter" || e.key === " ") && this.#active !== null) {
      this.write(assistEvent(this.#active, e.key === "Enter" ? 0 : 1));
      e.preventDefault();
    }
  }
}
