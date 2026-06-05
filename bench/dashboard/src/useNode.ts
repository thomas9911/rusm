import { useEffect, useReducer, useRef } from 'react';
import { encodeCommand, parseServerMessage } from './protocol';
import { applyMessage, initialState, setConnected, type DashboardState } from './state';
import type { ClientCommand, ServerMessage } from './types';

type Action =
  | { kind: 'message'; message: ServerMessage }
  | { kind: 'connected'; connected: boolean };

function reducer(state: DashboardState, action: Action): DashboardState {
  switch (action.kind) {
    case 'message':
      return applyMessage(state, action.message);
    case 'connected':
      return setConnected(state, action.connected);
  }
}

export interface NodeConnection {
  state: DashboardState;
  send: (command: ClientCommand) => void;
}

/** Connects to a RUSM node over WebSocket, reconnecting on drop. */
export function useNode(url: string): NodeConnection {
  const [state, dispatch] = useReducer(reducer, undefined, initialState);
  const socket = useRef<WebSocket | null>(null);

  useEffect(() => {
    let closed = false;
    let retry: ReturnType<typeof setTimeout>;

    const connect = () => {
      const ws = new WebSocket(url);
      socket.current = ws;
      ws.onopen = () => dispatch({ kind: 'connected', connected: true });
      ws.onmessage = (event) => {
        const message = parseServerMessage(event.data);
        if (message) dispatch({ kind: 'message', message });
      };
      ws.onclose = () => {
        dispatch({ kind: 'connected', connected: false });
        if (!closed) retry = setTimeout(connect, 1000);
      };
    };
    connect();

    return () => {
      closed = true;
      clearTimeout(retry);
      socket.current?.close();
    };
  }, [url]);

  const send = (command: ClientCommand) => {
    const ws = socket.current;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(encodeCommand(command));
  };

  return { state, send };
}
