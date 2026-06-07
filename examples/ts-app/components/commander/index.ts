/// <reference path="../../rusm.d.ts" />
import type * as Calc from "../calc/index";

// A RUSM **worker** component: RUSM runs `default` once. It spawns the `calc`
// service by name and calls it through the typed client — `spawn` + `send` +
// `receive` are all hidden; `await calc.add(...)` reads like a function call.
export default async function (): Promise<void> {
  const calc = spawn<typeof Calc>("calc");
  console.log("2 + 3 =", await calc.add(2, 3));
  console.log(await calc.greet({ name: "RUSM" }));
}
