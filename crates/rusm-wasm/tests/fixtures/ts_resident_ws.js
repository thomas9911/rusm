// A resident TS WebSocket "chat room": one instance serves every connection and
// holds the shared member list, so a frame from one broadcasts to all. The same
// `export default { websocket: { open, message, close } }` (Workers shape),
// lowered to CJS for the js-runner; `conn` is the connection's writer pid (BigInt).
const enc = new TextEncoder();
const members = [];
module.exports.default = {
  websocket: {
    open(conn) {
      members.push(conn);
      Process.send(conn, enc.encode("welcome"));
    },
    message(_conn, data) {
      for (const m of members) Process.send(m, data); // broadcast to all members
    },
    close(conn) {
      const i = members.indexOf(conn);
      if (i >= 0) members.splice(i, 1);
    },
  },
};
