// The semantic overlay: the program's tree of nodes (`semantics/tree`, std::semantics) as the real
// elements that carry each kind's behaviour, laid over its canvas. A link is an <a href>, so its
// menu, a middle click and Cmd+click work as on any page; a field is a real <input> in a <form>,
// so input methods, autofill and password managers work; headings, paragraphs, buttons, options,
// switches, sliders, tabs, menus and dialogs have their roles, states and the keyboard their
// patterns call for. The program says what exists; what a person does comes back as an `Assist`
// event, or a `Field` event for a field, by the node's number. The elements are invisible but
// where the program draws them (`semantics/geometry`, each frame they move), and a focused one
// shows a ring, for a person who moves through them by keyboard (F6 from the canvas; Escape back).
//
// A link takes the pointer over its own rectangle, so its native behaviour works; the wheel and
// the pointer's moves over it go on to the program as if over the canvas. A plain click on it is
// the program's (an Assist activation), exactly once. A field never takes the pointer: a press on
// its drawn rectangle gives it the keys, and the program places the caret with its own text's
// measure. The focus moving to a node is an Assist focus on it; leaving the overlay, on node 0.

import { assistEvent, fieldEvent } from "./input.ts";
import type { Forward, Overlay } from "./listen.ts";
import { Reader, Writer } from "./wire.ts";

/** A node's role (its byte on the wire), as std::semantics numbers them. */
export const Role = {
  Group: 0, Button: 1, ListBox: 2, Option: 3, Heading: 4, Text: 5, Link: 6, Code: 7, Field: 8,
  Checkbox: 9, Switch: 10, Slider: 11, TabList: 12, Tab: 13, TabPanel: 14, Dialog: 15, Menu: 16,
  MenuItem: 17, Table: 18, Row: 19, Cell: 20, Separator: 21,
} as const;
const ROLES = 22;

/** Roles that hold other nodes: their elements keep their place in the page's flow, so their
 * children are placed over the canvas as the root's are. Every other role holds its text. */
const CONTAINERS = new Set<number>([Role.Group, Role.ListBox, Role.TabList, Role.TabPanel, Role.Dialog, Role.Menu, Role.Table, Role.Row]);

/** A node's flags. */
export const PRESSED = 1, SELECTED = 2, DISABLED = 4, CHECKED = 8, EXPANDED = 16, MODAL = 32, FOCUSED = 64;

/** The Assist actions, as std::input numbers them. */
export const Action = { Activate: 0, Select: 1, Focus: 2, Increment: 3, Decrement: 4, Dismiss: 5 } as const;

/** A field's kinds, as std::semantics numbers them, and the element each is: email, URL and
 * numbers are text fields with the matching keyboard, so their selection can be read. */
const FIELD_KINDS: { type: string; mode: string | null }[] = [
  { type: "text", mode: null },
  { type: "password", mode: null },
  { type: "text", mode: "email" },
  { type: "search", mode: null },
  { type: "text", mode: "url" },
  { type: "tel", mode: null },
  { type: "text", mode: "decimal" },
  { type: "textarea", mode: null },
];

export interface FieldSpec {
  kind: number;
  spellcheck: boolean;
  form: number;
  replace: number;
  seen: number;
  start: number;
  end: number;
  name: string;
  autocomplete: string;
  text: string;
}

export interface Node {
  id: number;
  parent: number;
  role: number;
  flags: number;
  level: number;
  value: number;
  min: number;
  max: number;
  step: number;
  controls: number;
  label: string;
  href: string;
  field: FieldSpec | null;
}

/** At most this many nodes in a tree. */
const MOST = 16384;

/** A tree's bytes (std::semantics `structure`): its name, then each node, a parent before its
 * children. A bad tree fails whole. */
export function readTree(body: Uint8Array): { name: string; nodes: Node[] } {
  const r = new Reader(body);
  const name = r.str();
  const n = r.u32();
  if (n > MOST) throw new Error(`a semantic tree has at most ${MOST} nodes`);
  const nodes: Node[] = [];
  // Each node's role by number: only a container holds others.
  const roles = new Map<number, number>([[0, Role.Group]]);
  for (let i = 0; i < n; i++) {
    const id = r.u32(), parent = r.u32(), role = r.u8(), flags = r.u8(), level = r.u8();
    const [value, min, max, step] = [r.f32(), r.f32(), r.f32(), r.f32()];
    const controls = r.u32();
    const label = r.str(), href = r.str();
    let field: FieldSpec | null = null;
    if (r.u8() === 1) {
      const kind = r.u8(), spellcheck = r.u8() === 1;
      const [form, replace, seenRevision, start, end] = [r.u32(), r.u32(), r.u32(), r.u32(), r.u32()];
      field = { kind, spellcheck, form, replace, seen: seenRevision, start, end, name: r.str(), autocomplete: r.str(), text: r.str() };
      if (FIELD_KINDS[kind] === undefined) throw new Error(`no field kind ${kind}`);
    }
    if (id === 0 || roles.has(id)) throw new Error(`node ${id} is 0 or given twice`);
    const holder = roles.get(parent);
    if (holder === undefined) throw new Error(`node ${id}'s parent ${parent} comes after it or isn't there`);
    if (!CONTAINERS.has(holder)) throw new Error(`node ${id}'s parent ${parent} holds no nodes`);
    if (role >= ROLES) throw new Error(`no semantic role ${role}`);
    if (role === Role.Field && field === null) throw new Error(`field ${id} has no field`);
    roles.set(id, role);
    nodes.push({ id, parent, role, flags, level, value, min, max, step, controls, label, href, field });
  }
  return { name, nodes };
}

/** A geometry's bytes (std::semantics `geometry`): each node's rectangle, device pixels. */
export function readGeometry(body: Uint8Array): Map<number, [number, number, number, number]> {
  const r = new Reader(body);
  const n = r.u32();
  if (n > MOST) throw new Error(`a semantic tree has at most ${MOST} nodes`);
  const out = new Map<number, [number, number, number, number]>();
  for (let i = 0; i < n; i++) out.set(r.u32(), [r.f32(), r.f32(), r.f32(), r.f32()]);
  return out;
}

/** UTF-16 offset `i` of `s` as a UTF-8 byte offset, and back. */
export const utf8At = (s: string, i: number): number => new TextEncoder().encode(s.slice(0, i)).length;
export function utf16At(s: string, byte: number): number {
  let bytes = 0;
  for (let i = 0; i < s.length;) {
    if (bytes >= byte) return i;
    const c = s.codePointAt(i)!;
    bytes += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
    i += c >= 0x10000 ? 2 : 1;
  }
  return s.length;
}

/** Whether a field hands key `e` on to the program: leaving it, choosing, moving in a list, and
 * shortcuts that don't edit. A field's own editing keys, and an input method's, stay its own. */
export function forwards(e: KeyboardEvent, multiline: boolean): boolean {
  if (e.isComposing || e.keyCode === 229) return false;
  if (e.key === "Escape" || e.key === "PageUp" || e.key === "PageDown" || /^F\d+$/.test(e.key)) return true;
  if (!multiline && (e.key === "Enter" || e.key === "ArrowUp" || e.key === "ArrowDown")) return true;
  const editing = /^Key[ACVXYZ]$/.test(e.code) || e.key.startsWith("Arrow") || ["Backspace", "Delete", "Home", "End"].includes(e.key);
  return (e.ctrlKey || e.metaKey) && !editing && e.key.length === 1;
}

const STYLE = `
.wrela-semantics { position: fixed; pointer-events: none; z-index: 1; }
.wrela-semantics .wrela-node { position: absolute; margin: 0; padding: 0; border: 0; background: transparent;
  color: transparent; font-size: 1px; line-height: 1px; overflow: hidden; pointer-events: none; outline: none;
  white-space: pre; resize: none; caret-color: transparent; }
.wrela-semantics .wrela-holder { position: static; overflow: visible; }
.wrela-semantics a.wrela-node.wrela-hit { pointer-events: auto; cursor: pointer; }
.wrela-semantics .wrela-node:focus-visible, .wrela-semantics [role=listbox]:focus .wrela-active {
  outline: 3px solid #4766ff; outline-offset: 2px; border-radius: 4px; }
.wrela-semantics input.wrela-node:focus-visible, .wrela-semantics textarea.wrela-node:focus-visible { outline: none; }
.wrela-semantics .wrela-live { position: absolute; width: 1px; height: 1px; overflow: hidden;
  clip-path: inset(50%); white-space: nowrap; }
`;

/** A field's element and what the host knows of it. */
interface FieldState {
  el: HTMLInputElement | HTMLTextAreaElement;
  revision: number;
  seen: number;
  applied: number;
  composing: boolean;
  composeStart: number;
  composeText: string;
}


/** Roles named by `aria-label` rather than by their text. */
const NAMED = new Set<number>([...CONTAINERS, Role.Field, Role.Slider]);

export class Semantics implements Overlay {
  #root: HTMLDivElement | null = null;
  #live: HTMLDivElement | null = null;
  #els = new Map<number, HTMLElement>();
  #nodes = new Map<number, Node>();
  #fields = new Map<number, FieldState>();
  #forms = new Map<number, HTMLFormElement>();
  #forward: Forward | null = null;
  // The focus the program last asked for, and where the focus was before a modal dialog opened.
  #focused = 0;
  #returns = new Map<number, Element | null>();
  #renotify: ReturnType<typeof setTimeout> | null = null;

  /** `write` puts an event in the input ring; `place` gives the canvas's rectangle on the page and
   * its device pixels' size. */
  constructor(
    readonly canvas: HTMLCanvasElement,
    readonly write: (event: Uint8Array) => void,
    readonly place: () => { rect: DOMRect; width: number; height: number },
  ) {}

  /** What the overlay forwards, once the listener is there. */
  connect(forward: Forward): void {
    this.#forward = forward;
  }

  /** The overlay's root: a program that never sends a tree adds nothing to the page. */
  get root(): HTMLDivElement {
    if (this.#root) return this.#root;
    const style = document.createElement("style");
    style.textContent = STYLE;
    document.head.append(style);
    const root = Object.assign(document.createElement("div"), { className: "wrela-semantics" });
    const live = Object.assign(document.createElement("div"), { className: "wrela-live" });
    live.setAttribute("aria-live", "polite");
    root.append(live);
    document.body.append(root);
    root.addEventListener("keydown", (e) => this.#key(e));
    // The focus leaving the overlay is an Assist focus on node 0, so the program knows its node
    // lost it.
    root.addEventListener("focusout", (e) => {
      if (!this.contains(e.relatedTarget as Element | null)) this.write(assistEvent(0, Action.Focus));
    });
    document.addEventListener("selectionchange", () => {
      const a = document.activeElement;
      for (const [id, f] of this.#fields) if (f.el === a) this.#changed(id);
    });
    this.#root = root;
    this.#live = live;
    return root;
  }

  contains(e: Element | null): boolean {
    return e !== null && this.#root !== null && this.#root.contains(e);
  }

  /** Moves the keyboard's focus into the overlay: the focus the program asked for, else the first
   * element that takes it. */
  enter(): boolean {
    if (this.#root === null) return false;
    const asked = this.#els.get(this.#focused);
    const first = asked ?? this.#root.querySelector<HTMLElement>("[tabindex='0'], button, a, input, textarea");
    if (!first) return false;
    first.focus();
    return true;
  }

  focusAt(x: number, y: number): boolean {
    for (const f of this.#fields.values()) {
      const r = f.el.getBoundingClientRect();
      if (r.width > 0 && x >= r.left && x < r.right && y >= r.top && y < r.bottom) {
        if (document.activeElement !== f.el) f.el.focus({ preventScroll: true });
        return true;
      }
    }
    return false;
  }

  /** Says `text` to a screen reader, politely (after what it's saying). */
  announce(text: string): void {
    void this.root;
    const live = this.#live!;
    // A change is what's read: the same text twice is read twice.
    live.textContent = "";
    setTimeout(() => (live.textContent = text), 50);
  }

  /** The tree is now `body`'s. An element is kept by its node's number, and stays where it is in
   * the page unless the order changed; the focused element is never moved. */
  tree(body: Uint8Array): void {
    const { name, nodes } = readTree(body);
    const root = this.root;
    root.setAttribute("aria-label", name);
    // The tree's name is the page's: the browser's tab, its history and bookmarks show it.
    if (name) document.title = name;
    const seen = new Set<number>();
    const children = new Map<number, HTMLElement[]>();
    for (const n of nodes) {
      seen.add(n.id);
      const el = this.#element(n);
      this.#update(el, n);
      this.#nodes.set(n.id, n);
      const list = children.get(n.parent) ?? [];
      list.push(el);
      children.set(n.parent, list);
    }
    // Nodes that went: a focused one hands the focus back to the canvas.
    for (const [id, el] of this.#els) {
      if (seen.has(id)) continue;
      const focused = el.contains(document.activeElement);
      el.remove();
      this.#els.delete(id);
      this.#nodes.delete(id);
      this.#fields.delete(id);
      if (focused) this.canvas.focus();
    }
    for (const [form, el] of this.#forms) if (el.childElementCount === 0) { el.remove(); this.#forms.delete(form); }
    // Each parent's children, in order; the root's after its live region.
    for (const [parent, els] of children) {
      const holder = parent === 0 ? root : this.#els.get(parent)!;
      this.#order(holder, els, parent === 0 ? this.#live : null);
    }
    // A listbox's active option, for a screen reader: its first selected one.
    const active = new Map<number, number>();
    for (const n of nodes) if (n.role === Role.Option && n.flags & SELECTED && !active.has(n.parent)) active.set(n.parent, n.id);
    for (const n of nodes) {
      if (n.role !== Role.ListBox) continue;
      const option = active.get(n.id);
      const el = this.#els.get(n.id)!;
      if (option === undefined) el.removeAttribute("aria-activedescendant");
      else el.setAttribute("aria-activedescendant", `wrela-node-${option}`);
    }
    this.#dialogs(nodes);
    // The focus the program asked for, when it changes.
    const asked = nodes.find((n) => n.flags & FOCUSED)?.id ?? 0;
    if (asked !== this.#focused) {
      const was = this.#els.get(this.#focused);
      this.#focused = asked;
      const el = this.#els.get(asked);
      if (el && document.activeElement !== el) el.focus({ preventScroll: true });
      else if (!el && was !== undefined && was === document.activeElement) this.canvas.focus({ preventScroll: true });
    }
  }

  /** Where each node is now: its rectangle over the canvas, or out of reach. */
  geometry(body: Uint8Array): void {
    const rects = readGeometry(body);
    const { rect, width, height } = this.place();
    const root = this.root;
    Object.assign(root.style, { left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px` });
    const sx = width > 0 ? rect.width / width : 1;
    const sy = height > 0 ? rect.height / height : 1;
    for (const [id, [x, y, w, h]] of rects) {
      const el = this.#els.get(id);
      const n = this.#nodes.get(id);
      if (!el || !n || CONTAINERS.has(n.role)) continue;
      Object.assign(el.style, { left: `${x * sx}px`, top: `${y * sy}px`, width: `${w * sx}px`, height: `${h * sy}px` });
      // A link takes the pointer only where it's drawn and can be seen.
      el.classList.toggle("wrela-hit", w > 0 && h > 0);
    }
  }

  /** A field's state for the program (`field/state`): it has now seen this revision. */
  fieldState(node: number) {
    const f = this.#fields.get(node);
    if (!f) throw new Error(`node ${node} has no field`);
    const text = f.el.value;
    const start = f.el.selectionStart ?? text.length, end = f.el.selectionEnd ?? start;
    f.seen = Math.max(f.seen, f.revision);
    const compose = f.composing ? [utf8At(text, f.composeStart), utf8At(text, f.composeStart + f.composeText.length)] : [0, 0];
    return new Writer().u32(node).u32(f.revision).u32(utf8At(text, start)).u32(utf8At(text, end))
      .u8(f.el.selectionDirection === "backward" ? 1 : 0).u8(f.composing ? 1 : 0).u32(compose[0]!).u32(compose[1]!).str(text).bytes();
  }

  /** Submits form `form` the way a person would (`form/submit`), so a password manager offers to
   * save what's in it; the page doesn't go anywhere. */
  submit(form: number): void {
    const el = this.#forms.get(form);
    if (!el) throw new Error(`no form ${form}`);
    el.requestSubmit();
  }

  #element(n: Node): HTMLElement {
    const old = this.#els.get(n.id);
    const wanted = this.#tag(n);
    if (old && old.dataset.role === String(n.role) && old.tagName === wanted.toUpperCase() &&
      (n.role !== Role.Field || old.dataset.kind === String(n.field!.kind))) return old;
    old?.remove();
    const el = document.createElement(wanted);
    el.className = CONTAINERS.has(n.role) ? "wrela-node wrela-holder" : "wrela-node";
    el.dataset.role = String(n.role);
    el.id = `wrela-node-${n.id}`;
    const id = n.id;
    switch (n.role) {
      case Role.Button:
        el.addEventListener("click", () => this.write(assistEvent(id, Action.Activate)));
        break;
      case Role.Link: {
        el.addEventListener("click", (e) => {
          // A plain click is the program's; a middle or modified click, the browser's.
          if (e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
          e.preventDefault();
          this.write(assistEvent(id, Action.Activate));
        });
        el.addEventListener("wheel", (e) => this.#forward?.wheel(e), { passive: false });
        el.addEventListener("pointermove", (e) => this.#forward?.move(e));
        break;
      }
      case Role.Option: case Role.Tab: case Role.MenuItem:
        el.addEventListener("click", () => this.write(assistEvent(id, n.role === Role.MenuItem ? Action.Activate : Action.Select)));
        break;
      case Role.Field:
        this.#field(el as HTMLInputElement | HTMLTextAreaElement, n);
        break;
    }
    el.addEventListener("focus", () => this.write(assistEvent(id, Action.Focus)));
    this.#els.set(n.id, el);
    return el;
  }

  #tag(n: Node): string {
    switch (n.role) {
      case Role.Button: return "button";
      case Role.Link: return n.href ? "a" : "span";
      case Role.Text: case Role.Cell: return "p";
      case Role.Code: return "pre";
      case Role.Field: return FIELD_KINDS[n.field!.kind]!.type === "textarea" ? "textarea" : "input";
      case Role.Separator: return "hr";
      default: return "div";
    }
  }

  /** A field's element, in its form, and its listeners: each change is a new revision. */
  #field(el: HTMLInputElement | HTMLTextAreaElement, n: Node): void {
    const spec = n.field!;
    const kind = FIELD_KINDS[spec.kind]!;
    el.dataset.kind = String(spec.kind);
    if (kind.type !== "textarea") (el as HTMLInputElement).type = kind.type;
    if (kind.mode) el.setAttribute("inputmode", kind.mode);
    // A field made again counts on from the revision the program has read of the one before.
    const f: FieldState = { el, revision: spec.seen, seen: spec.seen, applied: 0, composing: false, composeStart: 0, composeText: "" };
    this.#fields.set(n.id, f);
    const id = n.id;
    el.addEventListener("input", () => this.#changed(id));
    el.addEventListener("compositionstart", () => {
      f.composing = true;
      f.composeStart = el.selectionStart ?? el.value.length;
      f.composeText = "";
      this.#changed(id);
    });
    el.addEventListener("compositionupdate", (e) => {
      f.composeText = (e as CompositionEvent).data ?? "";
      this.#changed(id);
    });
    el.addEventListener("compositionend", () => {
      f.composing = false;
      f.composeText = "";
      this.#changed(id);
    });
    // The keys a field doesn't use go on to the program; a list's arrows don't move the caret.
    const held = new Set<string>();
    const keys = el as HTMLElement;
    keys.addEventListener("keydown", (e) => {
      if (!forwards(e, el.tagName === "TEXTAREA")) return;
      held.add(e.code);
      this.#forward?.key(e, true);
      if (e.key === "ArrowUp" || e.key === "ArrowDown" || e.key === "Enter") e.preventDefault();
    });
    const up = (e: KeyboardEvent) => {
      if (held.delete(e.code)) this.#forward?.key(e, false);
    };
    keys.addEventListener("keyup", up);
  }

  /** Field `id` changed: a new revision, and the program hears of it until it reads it. */
  #changed(id: number): void {
    const f = this.#fields.get(id);
    if (!f) return;
    f.revision += 1;
    this.#notify();
  }

  /** Tells the program of each field with a revision it hasn't read; again a little later while
   * any are left, so a notice lost on a full input ring is said again. */
  #notify(): void {
    let left = false;
    for (const [id, f] of this.#fields) {
      if (f.revision <= f.seen) continue;
      left = true;
      this.write(fieldEvent(id, f.revision));
    }
    if (this.#renotify !== null) clearTimeout(this.#renotify);
    this.#renotify = left ? setTimeout(() => this.#notify(), 150) : null;
  }

  #update(el: HTMLElement, n: Node): void {
    const role = n.role;
    const flag = (bit: number) => (n.flags & bit) !== 0;
    const set = (k: string, v: string | null) => (v === null ? el.removeAttribute(k) : el.setAttribute(k, v));
    // What a node says is its text; a container's, a field's or a slider's is its name.
    if (NAMED.has(role)) set("aria-label", n.label || null);
    else if (role !== Role.Separator && el.textContent !== n.label) el.textContent = n.label;
    set("aria-disabled", flag(DISABLED) ? "true" : null);
    switch (role) {
      case Role.Group: set("role", "group"); break;
      case Role.Button: set("aria-pressed", flag(PRESSED) ? "true" : null); break;
      case Role.ListBox: set("role", "listbox"); el.tabIndex = 0; break;
      case Role.Option: set("role", "option"); set("aria-selected", String(flag(SELECTED))); el.classList.toggle("wrela-active", flag(SELECTED)); break;
      case Role.Heading: set("role", "heading"); set("aria-level", String(Math.min(Math.max(n.level, 1), 6))); break;
      case Role.Link:
        if (n.href) (el as HTMLAnchorElement).href = new URL(n.href, location.href).href;
        else { set("role", "link"); el.tabIndex = 0; }
        break;
      case Role.Field: this.#fieldUpdate(n); break;
      case Role.Checkbox: case Role.Switch:
        set("role", role === Role.Checkbox ? "checkbox" : "switch"); set("aria-checked", String(flag(CHECKED))); el.tabIndex = 0; break;
      case Role.Slider:
        set("role", "slider"); el.tabIndex = 0;
        set("aria-valuenow", String(n.value)); set("aria-valuemin", String(n.min)); set("aria-valuemax", String(n.max)); break;
      case Role.TabList: set("role", "tablist"); break;
      case Role.Tab:
        set("role", "tab"); set("aria-selected", String(flag(SELECTED)));
        set("aria-controls", n.controls ? `wrela-node-${n.controls}` : null);
        // One tab of a list takes the Tab key's focus: the selected one; the arrows move between.
        el.tabIndex = flag(SELECTED) ? 0 : -1;
        break;
      case Role.TabPanel: set("role", "tabpanel"); el.tabIndex = 0; break;
      case Role.Dialog: set("role", "dialog"); set("aria-modal", flag(MODAL) ? "true" : null); break;
      case Role.Menu: set("role", "menu"); break;
      case Role.MenuItem: set("role", "menuitem"); el.tabIndex = -1; break;
      case Role.Table: set("role", "table"); break;
      case Role.Row: set("role", "row"); break;
      case Role.Cell: set("role", "cell"); break;
    }
    if (role === Role.Group || role === Role.Option) set("aria-expanded", flag(EXPANDED) ? "true" : null);
  }

  /** A field's settings, and the program's replacement of its text if the field hasn't changed
   * since the program last read it and nothing is being composed. */
  #fieldUpdate(n: Node): void {
    const f = this.#fields.get(n.id)!;
    const spec = n.field!;
    const el = f.el;
    if (spec.name) el.name = spec.name; else el.removeAttribute("name");
    el.autocomplete = (spec.autocomplete || "off") as AutoFill;
    el.spellcheck = spec.spellcheck;
    // The form a field belongs to: a password manager saves a form's fields together.
    const form = this.#form(spec.form);
    if (el.parentElement !== form && form !== null) form.append(el);
    if (spec.replace > f.applied) {
      if (f.revision === spec.seen && !f.composing) {
        el.value = spec.text;
        const start = utf16At(spec.text, spec.start), end = utf16At(spec.text, spec.end);
        el.setSelectionRange(start, end);
        f.applied = spec.replace;
      }
    }
  }

  #form(id: number): HTMLFormElement | null {
    if (id === 0) return null;
    let form = this.#forms.get(id);
    if (!form) {
      form = document.createElement("form");
      form.addEventListener("submit", (e) => e.preventDefault());
      this.#forms.set(id, form);
      this.root.append(form);
    }
    return form;
  }

  /** Puts `els` in `holder` in order, moving only what's out of place, and never the focused
   * element or what holds it. */
  #order(holder: HTMLElement, els: HTMLElement[], after: Element | null): void {
    let ref: Element | null = after ? after.nextElementSibling : holder.firstElementChild;
    for (const el of els) {
      // A field lives in its form, which takes its place.
      const item: HTMLElement = el.parentElement?.tagName === "FORM" && el.parentElement !== holder ? el.parentElement : el;
      if (item === ref) { ref = ref.nextElementSibling; continue; }
      // Moving an element takes its focus away: one in its place already keeps it.
      if (item.parentElement === holder && item.contains(document.activeElement)) continue;
      holder.insertBefore(item, ref);
    }
  }

  /** A modal dialog that opened takes the focus, and gives it back where it was when it closes. */
  #dialogs(nodes: Node[]): void {
    const open = new Set(nodes.filter((n) => n.role === Role.Dialog && n.flags & MODAL).map((n) => n.id));
    for (const id of open) {
      if (this.#returns.has(id)) continue;
      this.#returns.set(id, document.activeElement);
      const first = this.#els.get(id)?.querySelector<HTMLElement>("button, a, input, textarea, [tabindex='0']");
      first?.focus();
    }
    for (const [id, back] of this.#returns) {
      if (open.has(id)) continue;
      this.#returns.delete(id);
      if (back !== null && back.isConnected) (back as HTMLElement).focus();
      else this.canvas.focus();
    }
  }

  /** The keyboard's patterns: Escape leaves the overlay or dismisses a dialog or a menu; the
   * arrows move among tabs, a menu's items and a listbox's options, and step a slider; Space and
   * Enter press a checkbox or a switch; Tab stays inside a modal dialog. */
  #key(e: KeyboardEvent): void {
    const el = document.activeElement as HTMLElement | null;
    if (!el || !this.#root?.contains(el)) return;
    const id = Number(el.id.replace("wrela-node-", ""));
    const n = this.#nodes.get(id);
    const role = n?.role ?? -1;
    const dialog = el.closest<HTMLElement>("[role=dialog]");
    const menu = el.closest<HTMLElement>("[role=menu]");
    if (e.key === "Escape") {
      if (role === Role.Field) return;
      const holder = dialog ?? menu;
      if (holder) this.write(assistEvent(Number(holder.id.replace("wrela-node-", "")), Action.Dismiss));
      else this.canvas.focus();
      e.preventDefault();
      return;
    }
    if (e.key === "Tab" && dialog?.getAttribute("aria-modal") === "true") {
      const stops = [...dialog.querySelectorAll<HTMLElement>("button, a, input, textarea, [tabindex='0']")];
      const i = stops.indexOf(el);
      const next = stops[(i + (e.shiftKey ? stops.length - 1 : 1)) % stops.length];
      next?.focus();
      e.preventDefault();
      return;
    }
    const step = (siblings: HTMLElement[], horizontal: boolean) => {
      const i = siblings.indexOf(el);
      const back = horizontal ? "ArrowLeft" : "ArrowUp", on = horizontal ? "ArrowRight" : "ArrowDown";
      const to = e.key === on ? (i + 1) % siblings.length : e.key === back ? (i + siblings.length - 1) % siblings.length :
        e.key === "Home" ? 0 : e.key === "End" ? siblings.length - 1 : -1;
      if (to < 0) return null;
      e.preventDefault();
      return siblings[to]!;
    };
    switch (role) {
      case Role.Tab: {
        const tabs = [...(el.parentElement?.children ?? [])].filter((c) => (c as HTMLElement).dataset.role === String(Role.Tab)) as HTMLElement[];
        const next = step(tabs, true);
        if (next) {
          next.focus();
          this.write(assistEvent(Number(next.id.replace("wrela-node-", "")), Action.Select));
        }
        break;
      }
      case Role.MenuItem: {
        const items = [...(el.parentElement?.children ?? [])].filter((c) => (c as HTMLElement).dataset.role === String(Role.MenuItem)) as HTMLElement[];
        const next = step(items, false);
        if (next) next.focus();
        else if (e.key === "Enter" || e.key === " ") { this.write(assistEvent(id, Action.Activate)); e.preventDefault(); }
        break;
      }
      case Role.ListBox: {
        const options = [...el.children].filter((c) => (c as HTMLElement).dataset.role === String(Role.Option)) as HTMLElement[];
        if (options.length === 0) break;
        const current = options.findIndex((o) => o.classList.contains("wrela-active"));
        const i = current < 0 ? -1 : current;
        const last = options.length - 1;
        const to = e.key === "ArrowDown" || e.key === "ArrowRight" ? Math.min(i + 1, last) :
          e.key === "ArrowUp" || e.key === "ArrowLeft" ? Math.max(i - 1, 0) : e.key === "Home" ? 0 : e.key === "End" ? last : null;
        if (to !== null) {
          this.write(assistEvent(Number(options[to]!.id.replace("wrela-node-", "")), Action.Select));
          e.preventDefault();
        } else if ((e.key === "Enter" || e.key === " ") && current >= 0) {
          this.write(assistEvent(Number(options[current]!.id.replace("wrela-node-", "")), e.key === "Enter" ? Action.Activate : Action.Select));
          e.preventDefault();
        }
        break;
      }
      case Role.Slider:
        if (e.key === "ArrowUp" || e.key === "ArrowRight") { this.write(assistEvent(id, Action.Increment)); e.preventDefault(); }
        if (e.key === "ArrowDown" || e.key === "ArrowLeft") { this.write(assistEvent(id, Action.Decrement)); e.preventDefault(); }
        break;
      case Role.Checkbox: case Role.Switch:
        if (e.key === " " || e.key === "Enter") { this.write(assistEvent(id, Action.Activate)); e.preventDefault(); }
        break;
    }
  }
}
