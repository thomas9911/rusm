/// <reference path="../../rusm.d.ts" />

// A RUSM **service** component: just export functions. RUSM runs the
// receive → dispatch → reply loop around them — no Process plumbing needed.
// A caller reaches these through the typed client (see ../commander/index.ts).

export function add(a: number, b: number): number {
  return a + b;
}

export async function greet({ name }: { name: string }): Promise<string> {
  return `hi ${name}`;
}
