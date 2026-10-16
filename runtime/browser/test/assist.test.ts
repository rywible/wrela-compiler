// The accessibility overlay (src/assist.ts): the program's tree as elements, and what an
// assistive technology does to them as Assist events by node.

import { expect, test } from "bun:test";
import { EVENT_SIZE } from "../src/abi.gen.ts";
import { DISABLED, Overlay, PRESSED, readTree, ROLE_BUTTON, ROLE_ITEM, SELECTED } from "../src/assist.ts";
import { assistEvent } from "../src/input.ts";
import { Writer } from "../src/wire.ts";
import { domEvent, type FakeElement, onPage } from "./page.ts";

type N = [id: number, role: number, flags: number, label: string];
const tree = (nodes: N[]) => {
  const w = new Writer().str("Board").str("Items").u32(nodes.length);
  nodes.forEach(([id, role, flags, label], i) => w.u32(id).u8(role).u8(flags).f32(10 * i).f32(20).f32(40).f32(40).str(label));
  return w.bytes();
};

test("a tree's bytes read back, and a bad one says why", () => {
  const t = readTree(tree([[7, ROLE_BUTTON, PRESSED, "Select"], [1001, ROLE_ITEM, SELECTED, "Sticky note: hi"]]));
  expect(t.name).toBe("Board");
  expect(t.nodes[1]).toEqual({ id: 1001, role: ROLE_ITEM, flags: SELECTED, x: 10, y: 20, w: 40, h: 40, label: "Sticky note: hi" });
  expect(() => readTree(tree([[1, 9, 0, "x"]]))).toThrow("no accessibility role 9");
  expect(() => readTree(tree([[1, ROLE_BUTTON, 0, "x"]]).subarray(0, 20))).toThrow("shorter");
});

function overlay(page: ReturnType<typeof Object>) {
  const events: Uint8Array[] = [];
  const canvas = (page as { createElement(t: string): FakeElement }).createElement("canvas");
  const o = new Overlay(canvas as unknown as HTMLCanvasElement, (e) => events.push(e), () => ({ rect: { left: 0, top: 0, width: 50, height: 50 } as DOMRect, width: 100, height: 100 }));
  return { o, events, canvas };
}

test("controls are buttons and items are options; each action names its node", () => onPage((page) => {
  const { o, events } = overlay(page);
  o.tree(tree([[7, ROLE_BUTTON, PRESSED, "Select"], [8, ROLE_BUTTON, DISABLED, "Undo"], [1001, ROLE_ITEM, 0, "A"], [1002, ROLE_ITEM, SELECTED, "B"]]));
  const [controls, list] = o.root.children as unknown as FakeElement[];
  const select = controls!.children[0]!;
  expect(select.tagName).toBe("BUTTON");
  expect(select.getAttribute("aria-pressed")).toBe("true");
  expect(controls!.children[1]!.getAttribute("aria-disabled")).toBe("true");
  expect(list!.getAttribute("role")).toBe("listbox");
  expect(list!.children.map((c) => c.getAttribute("aria-selected"))).toEqual(["false", "true"]);
  // Half the canvas's device pixels: the second node's left at 10 device pixels is at 5.
  expect(list!.children[0]!.style.left).toBe("10px");
  select.click();
  expect(events).toEqual([assistEvent(7, 0)]);
  // The arrows move the active option and select it; Enter activates it.
  list!.focus();
  list!.dispatchEvent(domEvent("keydown", { key: "ArrowDown" }));
  expect(list!.getAttribute("aria-activedescendant")).toBe("wrela-node-1001");
  expect(events[1]).toEqual(assistEvent(1001, 1));
  list!.dispatchEvent(domEvent("keydown", { key: "ArrowDown" }));
  list!.dispatchEvent(domEvent("keydown", { key: "Enter" }));
  expect(events.slice(2)).toEqual([assistEvent(1002, 1), assistEvent(1002, 0)]);
  expect(events.every((e) => e.length === EVENT_SIZE)).toBe(true);
}));

test("an update keeps each node's element, so the focus stays; a focused node that goes hands it on", () => onPage((page) => {
  const { o, canvas } = overlay(page);
  o.tree(tree([[7, ROLE_BUTTON, 0, "Select"], [1001, ROLE_ITEM, 0, "A"]]));
  const select = (o.root.children[0] as unknown as FakeElement).children[0]!;
  select.focus();
  o.tree(tree([[7, ROLE_BUTTON, PRESSED, "Select"], [1001, ROLE_ITEM, 0, "A (moved)"]]));
  expect((o.root.children[0] as unknown as FakeElement).children[0]).toBe(select);
  expect(page.activeElement).toBe(select);
  o.tree(tree([[1001, ROLE_ITEM, 0, "A"]]));
  expect(page.activeElement).toBe(o.root.children[1] as unknown as FakeElement);
  o.tree(tree([]));
  expect(o.enter()).toBe(false);
  (o.root.children[1] as unknown as FakeElement).focus();
  o.root.dispatchEvent(domEvent("keydown", { key: "Escape" }));
  expect(page.activeElement).toBe(canvas);
}));
