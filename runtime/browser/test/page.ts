// A page for the tests: just enough of `window` and `document` (events, elements, focus) for the
// input services (listen.ts) and the semantic overlay (semantics.ts). There's no DOM library
// here; each test swaps these in and puts the real globals back.

/** An element: events, attributes, children, style, focus and a few properties. */
export class FakeElement extends EventTarget {
  readonly attributes = new Map<string, string>();
  readonly children: FakeElement[] = [];
  readonly style: Record<string, string> & { cssText?: string } = {};
  readonly classes = new Set<string>();
  readonly dataset: Record<string, string> = {};
  parent: FakeElement | null = null;
  id = "";
  className = "";
  textContent = "";
  tabIndex = -1;
  value = "";
  autocomplete = "";
  spellcheck = true;
  type = "";
  name = "";
  href = "";
  selectionStart: number | null = null;
  selectionEnd: number | null = null;
  selectionDirection = "none";
  constructor(readonly page: FakePage, readonly tagName: string) {
    super();
  }
  /** Events go on to each parent, as most of the DOM's do. */
  override dispatchEvent(e: Event): boolean {
    super.dispatchEvent(e);
    if (this.parent) this.parent.dispatchEvent(e);
    else if (this.isConnected) this.page.dispatchEvent(e);
    return !e.defaultPrevented;
  }
  get isConnected(): boolean {
    let e: FakeElement = this;
    while (e.parent) e = e.parent;
    return e === this.page.body || e === this.page.head;
  }
  get parentElement(): FakeElement | null {
    return this.parent;
  }
  get firstElementChild(): FakeElement | null {
    return this.children[0] ?? null;
  }
  get nextElementSibling(): FakeElement | null {
    return this.parent?.children[this.parent.children.indexOf(this) + 1] ?? null;
  }
  insertBefore(x: FakeElement, ref: FakeElement | null): FakeElement {
    x.parent?.children.splice(x.parent.children.indexOf(x), 1);
    x.parent = this;
    const at = ref === null ? -1 : this.children.indexOf(ref);
    if (at < 0) this.children.push(x);
    else this.children.splice(at, 0, x);
    return x;
  }
  setSelectionRange(start: number, end: number): void {
    this.selectionStart = start;
    this.selectionEnd = end;
  }
  requestSubmit(): void {
    this.dispatchEvent(new Event("submit", { cancelable: true }));
  }
  /** Whether this matches `selector`: tags, `[attribute]` or `[attribute='value']`, or a list of them. */
  matches(selector: string): boolean {
    return selector.split(",").some((one) => {
      const m = /^\s*([a-z]*)(?:\[([a-z-]+)(?:=['"]?([^'"\]]*)['"]?)?\])?\s*$/i.exec(one);
      if (!m) throw new Error(`the fake page can't match ${one}`);
      const [, tag, attr, value] = m;
      if (tag && this.tagName !== tag.toUpperCase()) return false;
      if (!attr) return true;
      const have = attr === "tabindex" ? String(this.tabIndex) : attr === "id" ? this.id : this.getAttribute(attr);
      return value === undefined ? have !== null : have === value;
    });
  }
  closest(selector: string): FakeElement | null {
    for (let e: FakeElement | null = this; e !== null; e = e.parent) if (e.matches(selector)) return e;
    return null;
  }
  querySelectorAll(selector: string): FakeElement[] {
    return this.children.flatMap((c) => [...(c.matches(selector) ? [c] : []), ...c.querySelectorAll(selector)]);
  }
  setAttribute(k: string, v: string): void {
    this.attributes.set(k, v);
  }
  getAttribute(k: string): string | null {
    return this.attributes.get(k) ?? null;
  }
  removeAttribute(k: string): void {
    this.attributes.delete(k);
  }
  append(...xs: FakeElement[]): void {
    for (const x of xs) {
      x.parent?.children.splice(x.parent.children.indexOf(x), 1);
      x.parent = this;
      this.children.push(x);
    }
  }
  remove(): void {
    this.parent?.children.splice(this.parent.children.indexOf(this), 1);
    this.parent = null;
  }
  contains(x: FakeElement | null): boolean {
    for (let e = x; e !== null; e = e.parent) if (e === this) return true;
    return false;
  }
  get childElementCount(): number {
    return this.children.length;
  }
  querySelector(selector: string): FakeElement | null {
    return this.querySelectorAll(selector)[0] ?? null;
  }
  get classList() {
    return {
      toggle: (c: string, on: boolean) => (on ? this.classes.add(c) : this.classes.delete(c)),
      contains: (c: string) => this.classes.has(c),
    };
  }
  focus(): void {
    const was = this.page.activeElement;
    if (was === this) return;
    this.page.activeElement = this;
    if (was) was.dispatchEvent(Object.assign(new Event("focusout"), { relatedTarget: this }));
    super.dispatchEvent(new Event("focus"));
  }
  click(): void {
    this.dispatchEvent(new Event("click"));
  }
  /** Where its style places it (in pixels), or 0, 0 by 100 by 100. */
  getBoundingClientRect() {
    const px = (v: string | undefined, or: number) => (v === undefined ? or : Number.parseFloat(v));
    const [left, top, width, height] = [px(this.style.left, 0), px(this.style.top, 0), px(this.style.width, 100), px(this.style.height, 100)];
    return { left, top, width, height, right: left + width, bottom: top + height } as DOMRect;
  }
  setPointerCapture(): void {}
}

/** `window` and `document` together. */
export class FakePage extends EventTarget {
  activeElement: FakeElement | null = null;
  readonly body = new FakeElement(this, "BODY");
  readonly head = new FakeElement(this, "HEAD");
  /** Animation-frame callbacks waiting: `frame()` runs them. */
  readonly frames: (() => void)[] = [];
  createElement(tag: string): FakeElement {
    return new FakeElement(this, tag.toUpperCase());
  }
  frame(): void {
    for (const f of this.frames.splice(0)) f();
  }
}

/** Runs `body` with a fake page as `window` and `document` (and its animation frames and
 * address), then puts the real ones back: once it settles, if it's async. */
export function onPage<T>(body: (page: FakePage) => T): T {
  const names = ["window", "document", "requestAnimationFrame", "location"] as const;
  const saved = names.map((n) => Object.getOwnPropertyDescriptor(globalThis, n));
  const page = new FakePage();
  Object.defineProperty(globalThis, "window", { value: page, configurable: true });
  Object.defineProperty(globalThis, "document", { value: page, configurable: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (f: () => void) => page.frames.push(f), configurable: true });
  Object.defineProperty(globalThis, "location", { value: { href: "https://docs.example/v1/" }, configurable: true });
  const restore = () =>
    names.forEach((n, i) => {
      const d = saved[i];
      if (d) Object.defineProperty(globalThis, n, d);
      else delete (globalThis as Record<string, unknown>)[n];
    });
  let out: T;
  try {
    out = body(page);
  } catch (e) {
    restore();
    throw e;
  }
  // An async body has the page until it settles.
  if (out instanceof Promise) return out.finally(restore) as T;
  restore();
  return out;
}

/** A DOM event with these fields (`preventDefault` sets `defaultPrevented`, as a real one does). */
export function domEvent(type: string, fields: Record<string, unknown> = {}): Event {
  return Object.assign(new Event(type, { cancelable: true }), {
    shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, isComposing: false, keyCode: 0, ...fields,
  });
}
