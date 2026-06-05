import type { ClientCommand, ServerMessage } from './types';

/** Parses a server message, returning `null` for malformed or unknown input. */
export function parseServerMessage(text: string): ServerMessage | null {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof value !== 'object' || value === null) return null;
  const message = value as { type?: unknown };
  switch (message.type) {
    case 'hello':
    case 'tick':
    case 'error':
      return value as ServerMessage;
    default:
      return null;
  }
}

export const runCommand = (scenario: string): ClientCommand => ({ type: 'run', scenario });
export const stopCommand = (): ClientCommand => ({ type: 'stop' });
export const setObserverDetailCommand = (enabled: boolean): ClientCommand => ({
  type: 'set_observer_detail',
  enabled,
});

export function encodeCommand(command: ClientCommand): string {
  return JSON.stringify(command);
}
