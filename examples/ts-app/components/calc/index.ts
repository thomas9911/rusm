// A RUSM **service** component: just export functions. RUSM runs the
// receive → dispatch → reply loop around them — no Process plumbing needed.
// A caller reaches these through the typed client (see ../commander/index.ts).

export function add(a: number, b: number): number {
  return a + b;
}

export async function greet({ name }: { name: string }): Promise<string> {
  return `hi ${name}`;
}

// A streaming method: a generator's chunks ride a byte stream to the caller, who
// `for await`s them through the typed client.
export async function* countTo(n: number): AsyncGenerator<number> {
  for (let i = 1; i <= n; i++) yield i;
}

// A callback argument: `onProgress` stays in the caller; our invocations travel
// back as messages.
export async function work(onProgress: (pct: number) => void): Promise<string> {
  for (const pct of [25, 50, 100]) onProgress(pct);
  return "done";
}

// Publish the service's contract — derived from the functions above, so it can never
// drift from them. A caller imports this *type* (erased at build) to get a fully-typed
// client; `calc` stays a separate component, reached over messages, never bundled in.
export type Calc = typeof import(".");
