// The accessibility overlay: the program's accessibility tree (`a11y/tree`) as elements over its
// canvas, for screen readers and the keyboard. The program says what's there (controls, and
// items to choose from); the page gives them roles, names and focus, and an action taken on one
// goes back to the program as an `Assist` event by its node's number, never as a click on the
// canvas: what a node is doesn't depend on what's drawn over it, or on the program's mode.
//
// Controls are buttons; items are the options of one listbox, which keeps the focus and moves
// the active option with the arrow keys (selecting it, as a single choice does). A document's
// parts (headings, paragraphs, links and code) are a document region in the tree's order, its
// reading order, whether or not they are on screen: a link takes the focus, and its focus and
// its activation go to the program, which may scroll it into view. The elements are invisible
// but where the program draws them, and a focused one shows a ring, for a person who moves
// through them by keyboard (F6 from the canvas; Escape back).

import { assistEvent } from "./input.ts";
import { Reader } from "./wire.ts";

/** A node's role in the tree (its byte on the wire). */
export const ROLE_BUTTON = 0;
export const ROLE_ITEM = 1;
export const ROLE_HEADING = 2;
export const ROLE_TEXT = 3;
export const ROLE_LINK = 4;
export const ROLE_CODE = 5;
/** A node's flags. A heading's level, 1 to 6, is in bits 4 to 6. */
export const PRESSED = 1;
export const SELECTED = 2;
export const DISABLED = 4;
const LEVEL_SHIFT = 4;

/** The element a node of each role is, and where it goes: controls, the items' list, or the
 * document. */
const KINDS: { tag: string; role: string | null; part: "controls" | "list" | "doc" }[] = [
  { tag: "button", role: null, part: "controls" },
  { tag: "div", role: "option", part: "list" },
  { tag: "div", role: "heading", part: "doc" },
  { tag: "p", role: null, part: "doc" },
  { tag: "span", role: "link", part: "doc" },
  { tag: "pre", role: null, part: "doc" },
];

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
    if (KINDS[role] === undefined) throw new Error(`no accessibility role ${role}`);
    const level = (flags >> LEVEL_SHIFT) & 7;
    if (role === ROLE_HEADING && (level < 1 || level > 6)) throw new Error(`a heading's level is 1 to 6, not ${level}`);
    nodes.push({ id, role, flags, x, y, w, h, label });
  }
  return { name, items, nodes };
}

const STYLE = `
.wrela-a11y { position: fixed; pointer-events: none; z-index: 1; }
.wrela-a11y button, .wrela-a11y [role=option], .wrela-a11y .wrela-doc > * {
  position: absolute; margin: 0; padding: 0; border: 0; background: transparent;
  color: transparent; font-size: 1px; overflow: hidden; pointer-events: none; }
.wrela-a11y [role=listbox] { position: absolute; inset: 0; outline: none; }
.wrela-a11y button:focus-visible, .wrela-a11y [role=link]:focus-visible, .wrela-a11y [role=listbox]:focus .wrela-active {
  outline: 3px solid #4766ff; outline-offset: 2px; border-radius: 4px; }
.wrela-a11y .wrela-live { position: absolute; width: 1px; height: 1px; overflow: hidden;
  clip-path: inset(50%); white-space: nowrap; }
`;

/** The overlay's elements, made when the program first sends a tree or speaks. */
interface Parts {
  root: HTMLDivElement;
  controls: HTMLDivElement;
  list: HTMLDivElement;
  doc: HTMLDivElement;
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
    const parts = { root: make("wrela-a11y"), controls: make(""), list: make(""), doc: make("wrela-doc"), live: make("wrela-live") };
    parts.controls.setAttribute("role", "group");
    parts.list.setAttribute("role", "listbox");
    parts.list.tabIndex = 0;
    parts.doc.setAttribute("role", "document");
    parts.doc.tabIndex = -1;
    parts.live.setAttribute("aria-live", "polite");
    parts.root.append(parts.controls, parts.list, parts.doc, parts.live);
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

  /** Moves the keyboard's focus into the overlay: its first control, else its items, else its
   * document. */
  enter(): boolean {
    if (this.#parts === null) return false;
    const first = this.#parts.controls.querySelector("button");
    if (first) first.focus();
    else if (this.#order.length > 0) this.#parts.list.focus();
    else if (this.#parts.doc.childElementCount > 0) this.#parts.doc.focus();
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
    const parts = this.#dom;
    const { root, controls, list, doc } = parts;
    controls.setAttribute("aria-label", name);
    list.setAttribute("aria-label", items);
    doc.setAttribute("aria-label", name);
    const { rect, width, height } = this.place();
    Object.assign(root.style, { left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px` });
    const sx = width > 0 ? rect.width / width : 1;
    const sy = height > 0 ? rect.height / height : 1;
    const seen = new Set<number>();
    for (const n of nodes) {
      seen.add(n.id);
      let el = this.#nodes.get(n.id);
      const kind = KINDS[n.role]!;
      if (el && el.dataset.role !== String(n.role)) {
        el.remove();
        el = undefined;
      }
      if (!el) {
        el = document.createElement(kind.tag);
        el.id = `wrela-node-${n.id}`;
        el.dataset.role = String(n.role);
        if (kind.role) el.setAttribute("role", kind.role);
        if (n.role === ROLE_BUTTON) {
          el.tabIndex = 0;
          el.addEventListener("click", () => this.write(assistEvent(n.id, 0)));
        } else if (n.role === ROLE_ITEM) {
          el.addEventListener("click", () => this.#choose(n.id, 1));
        } else if (n.role === ROLE_LINK) {
          el.tabIndex = 0;
          el.addEventListener("click", () => this.write(assistEvent(n.id, 0)));
          el.addEventListener("keydown", (e) => {
            if (e.key !== "Enter") return;
            this.write(assistEvent(n.id, 0));
            e.preventDefault();
          });
          el.addEventListener("focus", () => this.write(assistEvent(n.id, 2)));
        }
        this.#nodes.set(n.id, el);
      }
      parts[kind.part].append(el);
      el.textContent = n.label;
      if (kind.part !== "doc") el.setAttribute("aria-label", n.label);
      Object.assign(el.style, { left: `${n.x * sx}px`, top: `${n.y * sy}px`, width: `${n.w * sx}px`, height: `${n.h * sy}px` });
      if (n.role === ROLE_BUTTON) {
        if (n.flags & PRESSED) el.setAttribute("aria-pressed", "true");
        else el.removeAttribute("aria-pressed");
        el.setAttribute("aria-disabled", String((n.flags & DISABLED) !== 0));
      } else if (n.role === ROLE_ITEM) {
        el.setAttribute("aria-selected", String((n.flags & SELECTED) !== 0));
      } else if (n.role === ROLE_HEADING) {
        el.setAttribute("aria-level", String((n.flags >> LEVEL_SHIFT) & 7));
      }
    }
    // Nodes that went: a focused one hands the focus to the items, or back to the canvas.
    for (const [id, el] of this.#nodes) {
      if (seen.has(id)) continue;
      const focused = el === document.activeElement;
      el.remove();
      this.#nodes.delete(id);
      if (focused) (list.childElementCount > 0 ? list : doc.childElementCount > 0 ? doc : this.canvas).focus();
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
