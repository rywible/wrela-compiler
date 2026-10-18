// The semantic overlay (src/semantics.ts): the program's tree as the elements that carry each
// kind's behaviour, and what a person does to them as Assist and Field events by node.

import { expect, test } from "bun:test";
import { assistEvent, fieldEvent, keyCode } from "../src/input.ts";
import type { Forward } from "../src/listen.ts";
import { Action, FOCUSED, forwards, MODAL, readTree, Role, SELECTED, Semantics, utf16At, utf8At } from "../src/semantics.ts";
import { Reader, Writer } from "../src/wire.ts";
import { domEvent, type FakeElement, type FakePage, onPage } from "./page.ts";

interface Spec {
  id: number;
  parent?: number;
  role: number;
  flags?: number;
  level?: number;
  value?: number;
  controls?: number;
  label?: string;
  href?: string;
  field?: { kind?: number; form?: number; replace?: number; seen?: number; start?: number; end?: number; name?: string; autocomplete?: string; text?: string };
}

/** A tree's bytes, as std::semantics `structure` writes them. */
function tree(nodes: Spec[], name = "Docs"): Uint8Array {
  const w = new Writer().str(name).u32(nodes.length);
  for (const n of nodes) {
    w.u32(n.id).u32(n.parent ?? 0).u8(n.role).u8(n.flags ?? 0).u8(n.level ?? 0);
    w.f32(n.value ?? 0).f32(0).f32(10).f32(1).u32(n.controls ?? 0).str(n.label ?? "").str(n.href ?? "");
    const f = n.field;
    if (!f) { w.u8(0); continue; }
    w.u8(1).u8(f.kind ?? 0).u8(0);
    for (const x of [f.form ?? 0, f.replace ?? 0, f.seen ?? 0, f.start ?? 0, f.end ?? 0]) w.u32(x);
    w.str(f.name ?? "").str(f.autocomplete ?? "").str(f.text ?? "");
  }
  return w.bytes();
}

/** A geometry's bytes: each node's rectangle, device pixels. */
function geometry(rects: [number, number, number, number, number][]): Uint8Array {
  const w = new Writer().u32(rects.length);
  for (const [id, x, y, width, height] of rects) w.u32(id).f32(x).f32(y).f32(width).f32(height);
  return w.bytes();
}

function overlay(page: FakePage) {
  const events: Uint8Array[] = [];
  const forwarded: string[] = [];
  const canvas = page.createElement("canvas");
  page.body.append(canvas);
  // The canvas is 50 by 50 CSS pixels and 100 by 100 device pixels.
  const o = new Semantics(canvas as unknown as HTMLCanvasElement, (e) => events.push(e), () => ({ rect: { left: 0, top: 0, width: 50, height: 50 } as DOMRect, width: 100, height: 100 }));
  const forward: Forward = {
    wheel: () => forwarded.push("wheel"),
    move: () => forwarded.push("move"),
    press: (e) => forwarded.push(`press ${e.pointerType}`),
    key: (e, down) => forwarded.push(`${down ? "down" : "up"} ${e.key}`),
  };
  o.connect(forward);
  const el = (id: number) => o.root.querySelectorAll(`[id='wrela-node-${id}']`)[0] as unknown as FakeElement;
  return { o, events, forwarded, canvas, el };
}

test("a tree's bytes read back, a parent before its children, and a bad one says why", () => {
  const t = readTree(tree([{ id: 1, role: Role.Group, label: "Page" }, { id: 2, parent: 1, role: Role.Heading, level: 2, label: "Arena" }]));
  expect(t.name).toBe("Docs");
  expect(t.nodes[1]).toMatchObject({ id: 2, parent: 1, role: Role.Heading, level: 2, label: "Arena", field: null });
  expect(() => readTree(tree([{ id: 2, parent: 1, role: Role.Text }]))).toThrow("comes after it");
  expect(() => readTree(tree([{ id: 1, role: Role.Text }, { id: 2, parent: 1, role: Role.Text }]))).toThrow("holds no nodes");
  expect(() => readTree(tree([{ id: 1, role: 22 }]))).toThrow("no semantic role 22");
  expect(() => readTree(tree([{ id: 1, role: Role.Text }, { id: 1, role: Role.Text }]))).toThrow("given twice");
  expect(() => readTree(tree([{ id: 1, role: Role.Field }]))).toThrow("has no field");
  expect(() => readTree(tree([{ id: 1, role: Role.Text, label: "x" }]).subarray(0, 20))).toThrow("shorter");
});

test("offsets go between UTF-16 and UTF-8", () => {
  const s = "aé😀b";
  expect([0, 1, 2, 4, 5].map((i) => utf8At(s, i))).toEqual([0, 1, 3, 7, 8]);
  expect([0, 1, 3, 7, 8, 99].map((b) => utf16At(s, b))).toEqual([0, 1, 2, 4, 5, 5]);
});

test("nodes are nested elements by parent: a document's parts, buttons and a list's options; the tree's name is the page's title", () => onPage((page) => {
  const { o, events, el } = overlay(page);
  o.tree(tree([
    { id: 1, role: Role.Group, label: "Page" },
    { id: 2, parent: 1, role: Role.Heading, level: 2, label: "Arena" },
    { id: 3, parent: 1, role: Role.Text, label: "Arenas hold values." },
    { id: 4, role: Role.Button, label: "Undo", flags: 4 },
    { id: 5, role: Role.ListBox, label: "Results" },
    { id: 6, parent: 5, role: Role.Option, label: "std::arena" },
    { id: 7, parent: 5, role: Role.Option, label: "std::io", flags: SELECTED },
  ]));
  expect(document.title).toBe("Docs");
  const page1 = el(1);
  expect(page1.children.map((c) => c.id)).toEqual(["wrela-node-2", "wrela-node-3"]);
  expect([el(2).getAttribute("role"), el(2).getAttribute("aria-level"), el(2).textContent]).toEqual(["heading", "2", "Arena"]);
  expect(el(3).tagName).toBe("P");
  expect(page1.getAttribute("aria-label")).toBe("Page");
  expect(el(4).tagName).toBe("BUTTON");
  expect(el(4).getAttribute("aria-disabled")).toBe("true");
  el(4).click();
  expect(events).toEqual([assistEvent(4, Action.Activate)]);
  expect(el(5).getAttribute("aria-activedescendant")).toBe("wrela-node-7");
  expect([el(6), el(7)].map((x) => x.getAttribute("aria-selected"))).toEqual(["false", "true"]);
  // The arrows select the next option; Enter activates the selected one.
  el(5).focus();
  events.length = 0;
  el(5).dispatchEvent(domEvent("keydown", { key: "ArrowUp" }));
  el(5).dispatchEvent(domEvent("keydown", { key: "Enter" }));
  expect(events).toEqual([assistEvent(6, Action.Select), assistEvent(7, Action.Activate)]);
}));

test("a link is an <a> to its address: a plain click is the program's, a modified one the browser's", () => onPage((page) => {
  const { o, events, forwarded, el } = overlay(page);
  o.tree(tree([{ id: 1, role: Role.Link, label: "std::io", href: "#std/io" }, { id: 2, role: Role.Link, label: "Run" }]));
  expect(el(1).tagName).toBe("A");
  // Against the page's address, not a release's <base>.
  expect(el(1).href).toBe("https://docs.example/v1/#std/io");
  expect(el(2).tagName).toBe("SPAN");
  expect(el(2).getAttribute("role")).toBe("link");
  const plain = domEvent("click", { button: 0 });
  el(1).dispatchEvent(plain);
  expect(plain.defaultPrevented).toBe(true);
  const modified = domEvent("click", { button: 0, metaKey: true });
  el(1).dispatchEvent(modified);
  expect(modified.defaultPrevented).toBe(false);
  expect(events).toEqual([assistEvent(1, Action.Activate)]);
  el(1).dispatchEvent(domEvent("wheel"));
  el(1).dispatchEvent(domEvent("pointermove"));
  expect(forwarded).toEqual(["wheel", "move"]);
  // A finger's press on a link is the canvas's, so a swipe from it scrolls; a mouse's is the
  // link's own.
  const finger = domEvent("pointerdown", { pointerType: "touch" });
  el(1).dispatchEvent(finger);
  el(1).dispatchEvent(domEvent("pointerdown", { pointerType: "mouse" }));
  expect(forwarded).toEqual(["wheel", "move", "press touch"]);
  expect(finger.defaultPrevented).toBe(true);
  // ...and the click a finger's tap ends in is the program's too, which had the press.
  events.length = 0;
  el(1).dispatchEvent(finger);
  const tapped = domEvent("click", { button: 0 });
  el(1).dispatchEvent(tapped);
  expect(tapped.defaultPrevented).toBe(true);
  expect(events).toEqual([]);
  // A link takes the pointer only where it's drawn: half the device pixels in CSS pixels.
  o.geometry(geometry([[1, 10, 20, 30, 8], [2, 0, 0, 0, 0]]));
  expect([el(1).style.left, el(1).style.top, el(1).style.width, el(1).style.height]).toEqual(["5px", "10px", "15px", "4px"]);
  expect([el(1).classes.has("wrela-hit"), el(2).classes.has("wrela-hit")]).toEqual([true, false]);
  // A geometry that doesn't move an element leaves it alone; one that does places it.
  el(1).style.left = "untouched";
  o.geometry(geometry([[1, 10, 20, 30, 8], [2, 0, 0, 0, 0]]));
  expect(el(1).style.left).toBe("untouched");
  o.geometry(geometry([[1, 10, 30, 30, 8], [2, 0, 0, 0, 0]]));
  expect([el(1).style.left, el(1).style.top]).toEqual(["5px", "15px"]);
}));

test("a link without an address activates from the keyboard", () => onPage((page) => {
  const { o, events, el } = overlay(page);
  o.tree(tree([{ id: 1, role: Role.Link, label: "Run" }]));
  el(1).focus();
  events.length = 0;
  const enter = domEvent("keydown", { key: "Enter" });
  el(1).dispatchEvent(enter);
  expect(events).toEqual([assistEvent(1, Action.Activate)]);
  expect(enter.defaultPrevented).toBe(true);
}));

test("checkboxes and switches activate from an assistive click", () => onPage((page) => {
  const { o, events, el } = overlay(page);
  o.tree(tree([{ id: 1, role: Role.Checkbox, label: "Accept" }, { id: 2, role: Role.Switch, label: "Sound" }]));
  el(1).click();
  el(2).click();
  expect(events).toEqual([assistEvent(1, Action.Activate), assistEvent(2, Action.Activate)]);
}));

test("an update keeps each element and its focus; the program's focus moves it; leaving says so", () => onPage((page) => {
  const { o, events, canvas, el } = overlay(page);
  o.tree(tree([{ id: 1, role: Role.Button, label: "A" }, { id: 2, role: Role.Button, label: "B" }]));
  const a = el(1);
  a.focus();
  o.tree(tree([{ id: 2, role: Role.Button, label: "B" }, { id: 1, role: Role.Button, label: "A, moved" }]));
  expect(el(1)).toBe(a);
  expect(page.activeElement).toBe(a);
  expect(a.textContent).toBe("A, moved");
  // The program asks for the focus on B, then on nothing: the canvas has it back.
  o.tree(tree([{ id: 2, role: Role.Button, label: "B", flags: FOCUSED }, { id: 1, role: Role.Button, label: "A" }]));
  expect(page.activeElement).toBe(el(2));
  events.length = 0;
  o.tree(tree([{ id: 2, role: Role.Button, label: "B" }, { id: 1, role: Role.Button, label: "A" }]));
  expect(page.activeElement).toBe(canvas);
  expect(events).toEqual([assistEvent(0, Action.Focus)]);
  // A focused node that goes hands the focus to the canvas.
  el(1).focus();
  o.tree(tree([{ id: 2, role: Role.Button, label: "B" }]));
  expect(page.activeElement).toBe(canvas);
  o.tree(tree([]));
  expect(o.enter()).toBe(false);
}));

test("a field is a real input in its form: each change is a revision the program hears of until it reads it", () => onPage(async (page) => {
  const { o, events, forwarded, el } = overlay(page);
  o.tree(tree([
    { id: 1, role: Role.Field, label: "Email", field: { kind: 2, form: 1, name: "email", autocomplete: "username" } },
    { id: 2, role: Role.Field, label: "Password", field: { kind: 1, form: 1, autocomplete: "current-password" } },
  ]));
  const email = el(1), password = el(2);
  expect([email.tagName, email.type, email.getAttribute("inputmode"), email.name, email.autocomplete]).toEqual(["INPUT", "text", "email", "email", "username"]);
  expect(password.type).toBe("password");
  expect(email.parentElement).toBe(password.parentElement);
  expect(email.parentElement!.tagName).toBe("FORM");
  // A press on its drawn rectangle gives it the keys.
  o.geometry(geometry([[1, 0, 0, 40, 10], [2, 0, 20, 40, 10]]));
  expect(o.focusAt(5, 2)).toBe(true);
  expect(page.activeElement).toBe(email);
  expect(o.focusAt(5, 30)).toBe(false);
  events.length = 0;
  email.value = "é@x";
  email.setSelectionRange(1, 1);
  email.dispatchEvent(domEvent("input"));
  expect(events).toEqual([fieldEvent(1, 1)]);
  // Said again until read; once read, not again.
  await new Promise((r) => setTimeout(r, 200));
  expect(events.length).toBeGreaterThan(1);
  const r = new Reader(o.fieldState(1));
  expect([r.u32(), r.u32(), r.u32(), r.u32(), r.u8(), r.u8(), r.u32(), r.u32(), r.str()]).toEqual([1, 1, 2, 2, 0, 0, 0, 0, "é@x"]);
  const heard = events.length;
  await new Promise((r) => setTimeout(r, 200));
  expect(events.length).toBe(heard);
  // The keys a field doesn't use go to the program; its editing keys stay its own.
  for (const [key, code] of [["Enter", "Enter"], ["a", "KeyA"], ["Escape", "Escape"]] as const) {
    email.dispatchEvent(domEvent("keydown", { key, code }));
    email.dispatchEvent(domEvent("keyup", { key, code }));
  }
  expect(forwarded).toEqual(["down Enter", "up Enter", "down Escape", "up Escape"]);
  // The form submits without leaving the page.
  const submitted: Event[] = [];
  email.parentElement!.addEventListener("submit", (e) => submitted.push(e));
  o.submit(1);
  expect(submitted[0]!.defaultPrevented).toBe(true);
}));

test("the program replaces a field's text only if it has read the field's newest revision and nothing is composing", () => onPage((page) => {
  const { o, el } = overlay(page);
  const field = (replace: number, seen: number, text: string, start = 0, end = 0) =>
    tree([{ id: 1, role: Role.Field, field: { replace, seen, text, start, end } }]);
  o.tree(field(1, 0, "héllo", 3, 3));
  const input = el(1);
  expect([input.value, input.selectionStart]).toEqual(["héllo", 2]);
  // A person types (revision 1) before the program has read it: its replacement waits.
  input.value = "héllo!";
  input.dispatchEvent(domEvent("input"));
  o.tree(field(2, 0, "x"));
  expect(input.value).toBe("héllo!");
  o.fieldState(1);
  o.tree(field(2, 1, "x", 1, 1));
  expect([input.value, input.selectionStart]).toEqual(["x", 1]);
  // Applied once: the same replacement again changes nothing.
  input.value = "xy";
  o.tree(field(2, 1, "x"));
  expect(input.value).toBe("xy");
  input.dispatchEvent(domEvent("compositionstart"));
  o.fieldState(1);
  o.tree(field(3, 3, "z"));
  expect(input.value).toBe("xy");
  // A field made again counts on from what the program has read, so its text goes in.
  o.tree(tree([]));
  o.tree(field(4, 3, "again", 5, 5));
  const made = el(1);
  expect(made).not.toBe(input);
  expect(made.value).toBe("again");
  made.value = "again!";
  made.dispatchEvent(domEvent("input"));
  expect(new DataView(o.fieldState(1).buffer).getUint32(4, true)).toBe(4);
}));

test("a node that changes from a field drops its old field state", () => onPage((page) => {
  const { o, el } = overlay(page);
  o.tree(tree([{ id: 1, role: Role.Field, field: { replace: 1, text: "old" } }]));
  const field = el(1);
  o.tree(tree([{ id: 1, role: Role.Button, label: "Continue" }]));
  expect(el(1)).not.toBe(field);
  expect(() => o.fieldState(1)).toThrow("node 1 has no field");
}));

test("a field can leave its form while keeping its element and text", () => onPage((page) => {
  const { o, el } = overlay(page);
  o.tree(tree([{ id: 1, role: Role.Field, field: { form: 1, replace: 1, text: "draft" } }]));
  const field = el(1);
  o.tree(tree([{ id: 1, role: Role.Field, field: { replace: 1 } }]));
  expect(el(1)).toBe(field);
  expect(field.value).toBe("draft");
  expect(field.parentElement === (o.root as unknown as FakeElement)).toBe(true);
  expect(() => o.submit(1)).toThrow("no form 1");
}));

test("tabs, sliders and a modal dialog have their keyboard patterns", () => onPage((page) => {
  const { o, events, el } = overlay(page);
  const opener = page.createElement("button");
  page.body.append(opener);
  opener.focus();
  o.tree(tree([
    { id: 1, role: Role.TabList, label: "Views" },
    { id: 2, parent: 1, role: Role.Tab, label: "Code", flags: SELECTED, controls: 9 },
    { id: 3, parent: 1, role: Role.Tab, label: "Docs" },
    { id: 4, role: Role.Slider, label: "Size", value: 3 },
  ]));
  expect([el(2).tabIndex, el(3).tabIndex, el(2).getAttribute("aria-controls")]).toEqual([0, -1, "wrela-node-9"]);
  el(2).focus();
  events.length = 0;
  el(2).dispatchEvent(domEvent("keydown", { key: "ArrowRight" }));
  expect(page.activeElement).toBe(el(3));
  el(4).focus();
  el(4).dispatchEvent(domEvent("keydown", { key: "ArrowUp" }));
  el(4).dispatchEvent(domEvent("keydown", { key: "ArrowLeft" }));
  expect(events).toEqual([
    assistEvent(3, Action.Focus), assistEvent(3, Action.Select), assistEvent(4, Action.Focus),
    assistEvent(4, Action.Increment), assistEvent(4, Action.Decrement),
  ]);
  expect(el(4).getAttribute("aria-valuenow")).toBe("3");
  // A modal dialog takes the focus, keeps Tab inside, is dismissed by Escape and gives the focus back.
  opener.focus();
  const dialog = (open: boolean) => tree(open ? [
    { id: 5, role: Role.Dialog, label: "Rename", flags: MODAL },
    { id: 6, parent: 5, role: Role.Button, label: "OK" },
    { id: 7, parent: 5, role: Role.Button, label: "Cancel" },
  ] : []);
  o.tree(dialog(true));
  expect(page.activeElement).toBe(el(6));
  el(7).focus();
  const tab = domEvent("keydown", { key: "Tab" });
  el(7).dispatchEvent(tab);
  expect([page.activeElement, tab.defaultPrevented]).toEqual([el(6), true]);
  events.length = 0;
  el(6).dispatchEvent(domEvent("keydown", { key: "Escape" }));
  expect(events).toEqual([assistEvent(5, Action.Dismiss)]);
  o.tree(dialog(false));
  expect(page.activeElement).toBe(opener);
}));

test("a field hands on the keys that leave it, choose or move in a list, and shortcuts that don't edit", () => {
  const key = (key: string, code: string, more: Record<string, unknown> = {}) =>
    domEvent("keydown", { key, code, ...more }) as KeyboardEvent;
  expect(forwards(key("Enter", "Enter"), false)).toBe(true);
  expect(forwards(key("Enter", "Enter"), true)).toBe(false);
  expect(forwards(key("ArrowDown", "ArrowDown"), false)).toBe(true);
  expect(forwards(key("ArrowLeft", "ArrowLeft"), false)).toBe(false);
  expect(forwards(key("k", "KeyK", { ctrlKey: true }), false)).toBe(true);
  expect(forwards(key("a", "KeyA", { ctrlKey: true }), false)).toBe(false);
  expect(forwards(key("Escape", "Escape", { isComposing: true }), false)).toBe(false);
  expect(keyCode("Escape")).toBeGreaterThan(0);
});
