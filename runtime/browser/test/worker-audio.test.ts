import { expect, test } from "bun:test";
import { failAudioRender } from "../src/worker-audio.ts";

test("an audio failure rejects the offline render barrier with its fatal error", async () => {
  const rendered = Promise.withResolvers<void>();
  const why = failAudioRender(rendered, "offline boom");

  expect(await rendered.promise.catch((e: unknown) => e)).toBe(why);
  expect(why.message).toBe("the voice trapped: offline boom");
});
