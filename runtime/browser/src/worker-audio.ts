/** Rejects the render worker's offline-audio barrier with the error its fatal path reports. */
export function failAudioRender(rendered: Pick<PromiseWithResolvers<void>, "reject">, message: string): Error {
  const why = new Error(`the voice trapped: ${message}`);
  rendered.reject(why);
  return why;
}
