// A resident TS WebSocket **echo** handler: one long-lived instance serves every
// connection and echoes each frame back to its sender — the TS twin of
// `rs_resident_ws_echo` (vs the broadcast `ts_resident_ws.js` chat room). The same
// `export default { websocket: { message } }` (Workers shape), lowered to CJS for
// the js-runner; `conn` is the connection's writer pid (BigInt).
module.exports.default = {
  websocket: {
    message(conn, data) {
      Process.send(conn, data); // echo to the sender only — O(1) per frame
    },
  },
};
