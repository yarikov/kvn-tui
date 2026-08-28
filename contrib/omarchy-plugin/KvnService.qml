import QtQuick
import Quickshell
import Quickshell.Io

// Single connection to the kvn-tui daemon: NDJSON over a Unix socket.
//
// The daemon pushes a full StateSnapshot after every state change (and about
// once per second with live traffic while connected), so the widget never
// polls. Commands are one-line JSON writes in the other direction. When the
// daemon is down the socket link is retried on a timer until it comes back.
Item {
  id: root

  // --- state mirrored from the latest snapshot -----------------------------
  property bool daemonUp: false
  // Idle | Connecting | ConnectPending | Connected
  property string connection: "Idle"
  property string statusText: ""
  property bool statusIsError: false
  property string activeProfileId: ""
  property string lastProfileId: ""
  // [{ id, name, protocol, latencyMs (number|null), testing }]
  property var profiles: []
  // Persisted geo routing state; the active mode is derived the same way the
  // Rust model derives it: selected_region_modes[current_region] ?? "global".
  property string geoRegion: "" // "ru" | "cn" | "ir" | "global" | "" (unset)
  property string routingMode: "global"
  property bool killSwitch: false
  property bool autoConnect: false
  property var traffic: ({ up: 0, down: 0, upTotal: 0, downTotal: 0, conns: 0 })

  readonly property bool connected: connection === "Connected"
  readonly property bool busy: connection === "Connecting" || connection === "ConnectPending"

  // --- socket plumbing ------------------------------------------------------
  property string uid: ""
  property bool linkEnabled: true

  readonly property string socketPath: {
    var xdg = Quickshell.env("XDG_RUNTIME_DIR") || ""
    if (xdg !== "") return xdg + "/kvn-tui.sock"
    return uid !== "" ? "/tmp/kvn-tui-" + uid + ".sock" : ""
  }

  function send(cmd) {
    if (!sock.connected) return false
    sock.write(JSON.stringify(cmd) + "\n")
    sock.flush()
    return true
  }

  function attach() { send({ cmd: "Attach" }) }
  function connectProfile(id) { send({ cmd: "ConnectProfile", profile_id: id }) }
  function disconnectVpn() { send({ cmd: "Disconnect" }) }
  function reconnect() { send({ cmd: "Reconnect" }) }
  function setRoutingMode(mode) { send({ cmd: "SetRoutingMode", mode: mode }) }
  function setGeoRegion(region) { send({ cmd: "SetGeoRegion", region: region }) }
  function setKillSwitch(enabled) { send({ cmd: "SetKillSwitch", enabled: enabled }) }
  function setAutoConnect(enabled) { send({ cmd: "SetAutoConnect", enabled: enabled }) }

  // Resolving the UID only matters when XDG_RUNTIME_DIR is unset (rare);
  // `id -u` runs once at startup.
  Process {
    command: ["id", "-u"]
    running: true
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.uid = String(text || "").trim()
    }
  }

  Socket {
    id: sock
    path: root.socketPath
    connected: root.socketPath !== "" && root.linkEnabled
    parser: SplitParser {
      onRead: function(data) { root.handleLine(data) }
    }
    onConnectedChanged: {
      root.daemonUp = connected
      if (connected) root.attach()
    }
    onError: function(error) {
      root.daemonUp = false
    }
  }

  // Flip the link off and on until the daemon answers. A failed connect
  // leaves the Socket dead — it does not retry on its own.
  Timer {
    interval: 5000
    repeat: true
    running: !root.daemonUp && root.socketPath !== ""
    onTriggered: {
      root.linkEnabled = false
      root.linkEnabled = true
    }
  }

  property string _profilesKey: ""

  function handleLine(data) {
    var snap
    try {
      snap = JSON.parse(String(data))
    } catch (e) {
      return
    }
    if (!snap || snap.connection === undefined) return

    connection = String(snap.connection)
    statusText = String(snap.status || "")
    statusIsError = snap.status_is_error === true
    activeProfileId = snap.active_profile_id ? String(snap.active_profile_id) : ""

    var settings = snap.settings || {}
    lastProfileId = settings.last_connected_profile
      ? String(settings.last_connected_profile) : ""

    // Rebuilding the profiles array on every traffic tick would reset the
    // list view; only rebuild when the roster actually changed.
    var roster = snap.profiles || []
    var key = ""
    for (var i = 0; i < roster.length; i++)
      key += roster[i].id + "|" + roster[i].name + "|" + roster[i].address + ";"
    var lat = snap.profile_latencies || {}
    var testing = snap.testing_profiles || []
    for (var j = 0; j < roster.length; j++) {
      var rid = String(roster[j].id)
      key += rid in lat ? (lat[rid] === null ? "n" : lat[rid]) : "-"
      key += testing.indexOf(rid) !== -1 ? "t" : ""
      key += ";"
    }
    if (key !== _profilesKey) {
      _profilesKey = key
      profiles = roster.map(function(p) {
        var pid = String(p.id)
        var hasLatency = lat !== null && pid in lat
        return {
          id: pid,
          name: String(p.name || ""),
          protocol: String(p.protocol || ""),
          latencyMs: hasLatency ? lat[pid] : undefined,
          testing: testing.indexOf(pid) !== -1
        }
      })
    }

    var gr = settings.geo_routing || {}
    geoRegion = gr.current_region ? String(gr.current_region) : ""
    routingMode = gr.current_region && gr.selected_region_modes
      ? String(gr.selected_region_modes[gr.current_region] || "global")
      : "global"

    killSwitch = settings.kill_switch === true
    autoConnect = settings.auto_connect === true

    var t = snap.traffic || {}
    traffic = {
      up: t.up_rate_bps || 0,
      down: t.down_rate_bps || 0,
      upTotal: t.up_total || 0,
      downTotal: t.down_total || 0,
      conns: t.conn_count || 0
    }
  }
}
