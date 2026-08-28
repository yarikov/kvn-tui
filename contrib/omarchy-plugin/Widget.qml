import QtQuick
import QtQuick.Layouts
import Quickshell
import qs.Commons
import qs.Ui

pragma ComponentBehavior: Bound

// kvn-tui bar widget.
//
// Icon states: dim shield = disconnected, full-color shield = connected,
// pulsing = connecting, urgent = last error.
//
// Left click: open the control panel (profiles + settings).
// Right click: quick toggle VPN (last profile) / disconnect.
// Middle click: open the full TUI.
BarWidget {
  id: root
  moduleName: "kvn.tui"

  KvnService {
    id: kvn
    onDaemonUpChanged: if (!daemonUp) root.close()
  }

  property bool popupOpen: false
  function close() { popupOpen = false }
  function toggle() { popupOpen = !popupOpen }

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  // All keyboard-selectable panel rows in order: profiles, then controls,
  // then settings, then the footer button.
  readonly property int profileCount: kvn.profiles.length
  readonly property int rowConnectLast: profileCount + 0
  readonly property int rowDisconnect: profileCount + 1
  readonly property int rowRouting: profileCount + 2
  readonly property int rowRegion: profileCount + 3
  readonly property int rowKillSwitch: profileCount + 4
  readonly property int rowAutoConnect: profileCount + 5
  readonly property int rowTui: profileCount + 6
  readonly property int rowCount: rowTui + 1
  property int cursorIndex: 0
  property bool cursorActive: false

  function activeProfile() {
    for (var i = 0; i < kvn.profiles.length; i++)
      if (kvn.profiles[i].id === kvn.activeProfileId) return kvn.profiles[i]
    return null
  }

  function lastProfile() {
    for (var i = 0; i < kvn.profiles.length; i++)
      if (kvn.profiles[i].id === kvn.lastProfileId) return kvn.profiles[i]
    return null
  }

  function startDaemon() {
    Quickshell.execDetached(["systemctl", "--user", "start", "kvn-tui.service"])
  }

  function openTui() {
    if (root.bar) root.bar.run("omarchy-launch-kvn-tui")
    else Quickshell.execDetached(["omarchy-launch-kvn-tui"])
    root.close()
  }

  function toggleVpn() {
    if (!kvn.daemonUp) {
      startDaemon()
      return
    }
    if (kvn.connected) {
      kvn.disconnectVpn()
    } else if (kvn.lastProfileId !== "") {
      kvn.connectProfile(kvn.lastProfileId)
    } else {
      root.toggle()
    }
  }

  // Geo region cycle order mirrors GeoRegion::ALL in the Rust model.
  readonly property var regionCycle: ["ru", "cn", "ir", "global"]

  function availableModes(region) {
    if (region !== "" && region !== "global")
      return ["global", "bypass_" + region, "only_" + region]
    return ["global"]
  }

  function modeLabel(mode) {
    if (mode === "global") return "Global"
    var parts = mode.split("_")
    var code = { "ru": "RU", "cn": "CN", "ir": "IR", "global": "Global" }[parts[1]] || parts[1].toUpperCase()
    return (parts[0] === "bypass" ? "Bypass " : "Only ") + code
  }

  function regionLabel(region) {
    if (region === "") return "Not set"
    return { "ru": "RU", "cn": "CN", "ir": "IR", "global": "Global" }[region] || region.toUpperCase()
  }

  function cycleMode(forward) {
    var modes = availableModes(kvn.geoRegion)
    var idx = modes.indexOf(kvn.routingMode)
    if (idx === -1) idx = 0
    var next = modes[(idx + (forward ? 1 : modes.length - 1)) % modes.length]
    if (next !== kvn.routingMode) kvn.setRoutingMode(next)
  }

  function cycleRegion(forward) {
    var idx = regionCycle.indexOf(kvn.geoRegion)
    if (idx === -1) idx = regionCycle.length - 1
    var next = regionCycle[(idx + (forward ? 1 : regionCycle.length - 1)) % regionCycle.length]
    if (next !== kvn.geoRegion) kvn.setGeoRegion(next)
  }

  function formatBytes(bytes) {
    var n = Number(bytes)
    if (!isFinite(n) || n < 0) n = 0
    if (n < 1024) return Math.round(n) + " B"
    if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB"
    if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MB"
    return (n / (1024 * 1024 * 1024)).toFixed(2) + " GB"
  }

  function formatRate(bytesPerSec) {
    return formatBytes(bytesPerSec) + "/s"
  }

  onPopupOpenChanged: {
    if (popupOpen) {
      cursorActive = false
      cursorIndex = 0
      if (kvn.connected && kvn.activeProfileId !== "") {
        for (var i = 0; i < kvn.profiles.length; i++)
          if (kvn.profiles[i].id === kvn.activeProfileId) { cursorIndex = i; break }
      }
      Qt.callLater(function() { if (root.popupOpen) keyCatcher.forceActiveFocus() })
    }
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: kvn.connected ? "󰦝" : "󰦜"
    fontFamily: root.fontFamily
    tooltipText: {
      if (!kvn.daemonUp) return "kvn-tui — daemon not running (click to start)"
      if (kvn.connected) {
        var p = root.activeProfile()
        return "kvn-tui — connected to " + (p ? p.name : "profile")
      }
      if (kvn.busy) return "kvn-tui — connecting…"
      return "kvn-tui — disconnected"
    }
    foreground: root.foreground
    activeColor: root.foreground
    useActiveColor: false
    active: kvn.connected
    dimmed: !kvn.connected && !kvn.busy
    onPressed: function(b) {
      if (b === Qt.LeftButton) {
        root.toggle()
      } else if (b === Qt.MiddleButton) {
        root.openTui()
      } else {
        root.toggleVpn()
      }
    }

    // Connecting: gentle opacity pulse.
    opacity: 1
    SequentialAnimation on opacity {
      running: kvn.busy
      loops: Animation.Infinite
      NumberAnimation { to: 0.35; duration: 550; easing.type: Easing.InOutQuad }
      NumberAnimation { to: 1; duration: 550; easing.type: Easing.InOutQuad }
      onRunningChanged: if (!running) button.opacity = 1
    }
  }

  KeyboardPanel {
    id: popup
    anchorItem: button
    bar: root.bar
    owner: root
    open: root.popupOpen
    focusTarget: keyCatcher
    contentWidth: fittedContentWidth(Style.space(340))
    contentHeight: root.popupOpen && kvn.daemonUp && root.profileCount > 0
      ? cappedContentHeight(Style.space(600))
      : fittedContentHeight(mainColumn.implicitHeight)

    ColumnLayout {
      id: mainColumn
      anchors.fill: parent
      spacing: Style.space(6)

      // --- header ---
      RowLayout {
        Layout.fillWidth: true
        spacing: Style.space(8)

        Text {
          text: kvn.connected ? "󰦝" : "󰦜"
          color: !kvn.daemonUp || kvn.statusIsError ? Color.urgent
            : (kvn.connected ? root.foreground : root.dim)
          font.family: root.fontFamily
          font.pixelSize: Style.font.title
        }

        Text {
          text: "kvn-tui"
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.title
          Layout.fillWidth: true
        }
      }

      // --- daemon offline ---
      ColumnLayout {
        Layout.fillWidth: true
        spacing: Style.space(10)
        visible: !kvn.daemonUp

        Text {
          Layout.fillWidth: true
          text: "The kvn-tui daemon is not running. Start it to control the VPN from the bar."
          color: root.dim
          wrapMode: Text.Wrap
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
        }

        Rectangle {
          Layout.fillWidth: true
          Layout.preferredHeight: Style.space(36)
          radius: Style.cornerRadius
          color: startArea.containsMouse ? Qt.lighter(Color.menu.selectedBackground, 1.15) : Color.menu.selectedBackground

          Text {
            anchors.centerIn: parent
            text: "Start daemon"
            color: Color.menu.selectedText
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
          }

          MouseArea {
            id: startArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.startDaemon()
          }
        }
      }

      // --- main panel ---
      ColumnLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        spacing: Style.space(6)
        visible: kvn.daemonUp

        // status line
        Text {
          Layout.fillWidth: true
          visible: kvn.statusText !== ""
          text: kvn.statusText
          color: kvn.statusIsError ? Color.urgent : root.dim
          wrapMode: Text.Wrap
          elide: Text.ElideRight
          maximumLineCount: 2
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }

        // Connection details grid — same shape as the Omarchy network panel.
        GridLayout {
          Layout.fillWidth: true
          visible: kvn.connected
          columns: 4
          columnSpacing: Style.space(20)
          rowSpacing: Style.spacing.labelGap

          InfoLabel { text: "Receiving" }
          DetailValue { text: root.formatRate(kvn.traffic.down) }
          InfoLabel { text: "Sending" }
          DetailValue { text: root.formatRate(kvn.traffic.up) }

          InfoLabel { text: "Downloaded" }
          DetailValue { text: root.formatBytes(kvn.traffic.downTotal) }
          InfoLabel { text: "Uploaded" }
          DetailValue { text: root.formatBytes(kvn.traffic.upTotal) }
        }

        // --- connection controls ---
        RowLayout {
          Layout.fillWidth: true
          spacing: Style.space(6)

          ControlButton {
            id: connectLastButton
            Layout.fillWidth: true
            glyph: "▶"
            label: {
              var p = root.lastProfile()
              return p ? "Connect " + p.name : "Connect last"
            }
            enabled: kvn.lastProfileId !== "" && !kvn.connected
            rowIndex: root.rowConnectLast
            primary: true
            onActivated: if (kvn.lastProfileId !== "") kvn.connectProfile(kvn.lastProfileId)
          }

          ControlButton {
            Layout.fillWidth: true
            glyph: "■"
            label: "Disconnect"
            enabled: kvn.connected
            rowIndex: root.rowDisconnect
            onActivated: kvn.disconnectVpn()
          }
        }

        PanelSectionHeader {
          Layout.fillWidth: true
          text: "Profiles"
          foreground: root.dim
          fontFamily: root.fontFamily
        }

        // profile list
        ListView {
          id: profileList
          Layout.fillWidth: true
          Layout.fillHeight: true
          Layout.preferredHeight: Style.space(180)
          implicitHeight: contentHeight
          visible: root.profileCount > 0
          clip: true
          boundsBehavior: Flickable.StopAtBounds
          spacing: Style.space(2)
          model: kvn.profiles

          delegate: Rectangle {
            id: profileRow
            required property int index
            required property var modelData
            readonly property bool isActive: modelData.id === kvn.activeProfileId
            readonly property bool isCursor: root.cursorActive && root.cursorIndex === index
            width: profileList.width
            height: Style.space(34)
            radius: Style.cornerRadius
            color: isCursor ? Color.menu.selectedBackground
              : (rowHover.containsMouse ? Util.alpha(root.foreground, 0.06) : "transparent")
            Behavior on color { ColorAnimation { duration: 80 } }

            MouseArea {
              id: rowHover
              anchors.fill: parent
              hoverEnabled: true
              cursorShape: Qt.PointingHandCursor
              onContainsMouseChanged: if (containsMouse) {
                root.cursorActive = true
                root.cursorIndex = profileRow.index
              }
              onClicked: {
                if (!profileRow.isActive) kvn.connectProfile(profileRow.modelData.id)
              }
            }

            RowLayout {
              anchors.fill: parent
              anchors.leftMargin: Style.space(10)
              anchors.rightMargin: Style.space(10)
              spacing: Style.space(8)

              Text {
                text: profileRow.isActive ? "●" : " "
                color: Color.accent
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }

              Text {
                Layout.fillWidth: true
                text: profileRow.modelData.name
                color: profileRow.isCursor ? Color.menu.selectedText : root.foreground
                elide: Text.ElideMiddle
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                font.bold: profileRow.isActive
              }

              Text {
                text: profileRow.modelData.protocol
                color: profileRow.isCursor ? Color.menu.selectedText : root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                visible: text !== ""
              }

              Text {
                text: profileRow.modelData.testing ? "…"
                  : profileRow.modelData.latencyMs === undefined ? ""
                  : profileRow.modelData.latencyMs === null ? "unreachable"
                  : profileRow.modelData.latencyMs + " ms"
                color: profileRow.modelData.latencyMs === null && !profileRow.modelData.testing ? Color.urgent : root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }

          Text {
            anchors.centerIn: parent
            width: parent.width - Style.space(16)
            visible: root.profileCount === 0
            text: "No profiles. Add them with `kvn-tui` (p to paste a link)."
            color: root.dim
            wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignHCenter
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }
        }

        PanelSectionHeader {
          Layout.fillWidth: true
          text: "Settings"
          foreground: root.dim
          fontFamily: root.fontFamily
        }

        PanelRow {
          Layout.fillWidth: true
          rowHeight: Style.space(30)
          glyph: ""
          label: "Routing"
          valueText: "‹ " + root.modeLabel(kvn.routingMode) + " ›"
          rowIndex: root.rowRouting
          onActivated: root.cycleMode(true)
          onStep: function(forward) { root.cycleMode(forward) }
        }

        PanelRow {
          Layout.fillWidth: true
          rowHeight: Style.space(30)
          glyph: ""
          label: "Region"
          valueText: "‹ " + root.regionLabel(kvn.geoRegion) + " ›"
          rowIndex: root.rowRegion
          onActivated: root.cycleRegion(true)
          onStep: function(forward) { root.cycleRegion(forward) }
        }

        PanelRow {
          Layout.fillWidth: true
          rowHeight: Style.space(30)
          glyph: ""
          label: "Kill switch"
          showToggle: true
          toggleChecked: kvn.killSwitch
          toggleBusy: ksPending
          rowIndex: root.rowKillSwitch
          onActivated: {
            ksRequested = !kvn.killSwitch
            kvn.setKillSwitch(ksRequested)
            ksTimeout.restart()
          }
        }

        PanelRow {
          Layout.fillWidth: true
          rowHeight: Style.space(30)
          glyph: ""
          label: "Auto-connect"
          showToggle: true
          toggleChecked: kvn.autoConnect
          rowIndex: root.rowAutoConnect
          onActivated: kvn.setAutoConnect(!kvn.autoConnect)
        }

        // Footer: open the full TUI
        Rectangle {
          Layout.fillWidth: true
          Layout.preferredHeight: Style.space(30)
          radius: Style.cornerRadius
          color: tuiArea.containsMouse || (root.cursorActive && root.cursorIndex === root.rowTui)
            ? Qt.lighter(Color.menu.selectedBackground, 1.15)
            : Util.alpha(root.foreground, 0.06)

          Text {
            anchors.centerIn: parent
            text: "Open TUI"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }

          MouseArea {
            id: tuiArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onContainsMouseChanged: if (containsMouse) {
              root.cursorActive = true
              root.cursorIndex = root.rowTui
            }
            onClicked: root.openTui()
          }
        }

        // Hidden focus catcher for keyboard navigation. Must live inside the
        // panel content — the popup is its own window, so key events are
        // delivered to the focused item within it.
        Item {
          id: keyCatcher
          width: 0
          height: 0
          focus: true
          Keys.priority: Keys.BeforeItem
          Keys.onPressed: function(event) {
            if (event.key === Qt.Key_Escape) {
              root.close()
              event.accepted = true
              return
            }
            if (!kvn.daemonUp) {
              if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                root.startDaemon()
                event.accepted = true
              }
              return
            }
            if (event.key === Qt.Key_J || event.key === Qt.Key_Down) {
              root.moveCursor(1)
              event.accepted = true
            } else if (event.key === Qt.Key_K || event.key === Qt.Key_Up) {
              root.moveCursor(-1)
              event.accepted = true
            } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space) {
              root.activateRow(root.cursorIndex)
              event.accepted = true
            } else if (event.key === Qt.Key_L || event.key === Qt.Key_Right) {
              root.stepRow(root.cursorIndex, true)
              event.accepted = true
            } else if (event.key === Qt.Key_H || event.key === Qt.Key_Left) {
              root.stepRow(root.cursorIndex, false)
              event.accepted = true
            } else if (event.text === "G") {
              root.cursorActive = true
              root.cursorIndex = root.rowCount - 1
              root.syncCursorView()
              event.accepted = true
            } else if (event.key === Qt.Key_G) {
              root.cursorActive = true
              root.cursorIndex = 0
              root.syncCursorView()
              event.accepted = true
            } else if (event.text === "K") {
              root.activateRow(root.rowKillSwitch)
              event.accepted = true
            } else if (event.text === "s") {
              if (kvn.connected) kvn.disconnectVpn()
              event.accepted = true
            } else if (event.text === "r") {
              if (kvn.connected) kvn.reconnect()
              event.accepted = true
            } else if (event.text === "a") {
              kvn.setAutoConnect(!kvn.autoConnect)
              event.accepted = true
            } else if (event.text === "t") {
              root.openTui()
              event.accepted = true
            }
          }
        }
      }
    }
  }

  // Kill-switch toggle feedback: `busy` until the daemon confirms the new
  // state (or the request times out — a failed toggle never flips).
  property bool ksRequested: false
  property bool ksPending: kvn.killSwitch !== ksRequested && ksTimeout.running
  Timer {
    id: ksTimeout
    interval: 6000
    onTriggered: root.ksRequested = kvn.killSwitch
  }

  function moveCursor(delta) {
    if (rowCount === 0) return
    if (!cursorActive) {
      cursorActive = true
      cursorIndex = delta < 0 ? rowCount - 1 : 0
    } else {
      cursorIndex = (cursorIndex + delta + rowCount) % rowCount
    }
    syncCursorView()
  }

  function syncCursorView() {
    if (cursorIndex < profileCount)
      profileList.positionViewAtIndex(cursorIndex, ListView.Contain)
  }

  function activateRow(index) {
    if (index < profileCount) {
      if (kvn.profiles[index] && kvn.profiles[index].id !== kvn.activeProfileId)
        kvn.connectProfile(kvn.profiles[index].id)
    } else if (index === rowConnectLast) {
      if (kvn.lastProfileId !== "" && !kvn.connected) kvn.connectProfile(kvn.lastProfileId)
    } else if (index === rowDisconnect) {
      if (kvn.connected) kvn.disconnectVpn()
    } else if (index === rowRouting) {
      cycleMode(true)
    } else if (index === rowRegion) {
      cycleRegion(true)
    } else if (index === rowKillSwitch) {
      ksRequested = !kvn.killSwitch
      kvn.setKillSwitch(ksRequested)
      ksTimeout.restart()
    } else if (index === rowAutoConnect) {
      kvn.setAutoConnect(!kvn.autoConnect)
    } else if (index === rowTui) {
      openTui()
    }
  }

  function stepRow(index, forward) {
    if (index === rowRouting) cycleMode(forward)
    else if (index === rowRegion) cycleRegion(forward)
  }

  // Dim label / right-aligned value pair, styled like the Omarchy network
  // panel's connection-details grid.
  component InfoLabel: Text {
    color: root.foreground
    opacity: 0.6
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
  }

  component DetailValue: Text {
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    Layout.fillWidth: true
    horizontalAlignment: Text.AlignRight
  }

  // Connection control button (Connect / Disconnect).
  component ControlButton: Rectangle {
    id: control
    signal activated()
    property string glyph: ""
    property string label: ""
    property bool enabled: true
    property bool primary: false
    property int rowIndex: -1
    readonly property bool isCursor: root.cursorActive && root.cursorIndex === rowIndex
    readonly property bool highlighted: isCursor || (hover.containsMouse && enabled)

    Layout.fillWidth: true
    Layout.preferredHeight: Style.space(34)
    radius: Style.cornerRadius
    color: !enabled ? "transparent"
      : highlighted ? (primary ? Qt.lighter(Color.menu.selectedBackground, 1.15) : Util.alpha(root.foreground, 0.1))
      : primary ? Color.menu.selectedBackground
      : Util.alpha(root.foreground, 0.06)
    opacity: enabled ? 1 : 0.4
    Behavior on color { ColorAnimation { duration: 80 } }

    MouseArea {
      id: hover
      anchors.fill: parent
      hoverEnabled: true
      enabled: control.enabled
      cursorShape: Qt.PointingHandCursor
      onContainsMouseChanged: if (containsMouse) {
        root.cursorActive = true
        root.cursorIndex = control.rowIndex
      }
      onClicked: control.activated()
    }

    RowLayout {
      anchors.centerIn: parent
      spacing: Style.space(6)
      width: parent.width - Style.space(12)

      Text {
        text: control.glyph
        visible: control.glyph !== ""
        color: control.primary && control.highlighted ? Color.menu.selectedText
          : (control.primary ? Color.menu.selectedText : root.dim)
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }

      Text {
        Layout.fillWidth: true
        text: control.label
        color: control.primary ? Color.menu.selectedText : root.foreground
        elide: Text.ElideMiddle
        horizontalAlignment: Text.AlignHCenter
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }
    }
  }

  // Selectable settings row: cycler or toggle.
  component PanelRow: Rectangle {
    id: row
    signal activated()
    signal step(bool forward)
    property string glyph: ""
    property string label: ""
    property string valueText: ""
    property bool rowEnabled: true
    property int rowHeight: Style.space(34)
    property bool showToggle: false
    property bool toggleChecked: false
    property bool toggleBusy: false
    property int rowIndex: -1
    readonly property bool isCursor: root.cursorActive && root.cursorIndex === rowIndex

    Layout.fillWidth: true
    Layout.preferredHeight: rowHeight
    radius: Style.cornerRadius
    color: isCursor ? Color.menu.selectedBackground
      : (rowArea.containsMouse && rowEnabled ? Util.alpha(root.foreground, 0.06) : "transparent")
    opacity: rowEnabled ? 1 : 0.4
    Behavior on color { ColorAnimation { duration: 80 } }

    MouseArea {
      id: rowArea
      anchors.fill: parent
      hoverEnabled: true
      enabled: row.rowEnabled
      cursorShape: Qt.PointingHandCursor
      onContainsMouseChanged: if (containsMouse) {
        root.cursorActive = true
        root.cursorIndex = row.rowIndex
      }
      onClicked: row.activated()
    }

    RowLayout {
      anchors.fill: parent
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(10)
      spacing: Style.space(8)

      Text {
        text: row.glyph
        visible: row.glyph !== ""
        color: row.isCursor ? Color.menu.selectedText : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }

      Text {
        Layout.fillWidth: true
        text: row.label
        color: row.isCursor ? Color.menu.selectedText : root.foreground
        elide: Text.ElideMiddle
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
      }

      Text {
        visible: !row.showToggle && row.valueText !== ""
        text: row.valueText
        color: row.isCursor ? Color.menu.selectedText : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
      }

      ToggleSwitch {
        visible: row.showToggle
        checked: row.toggleChecked
        busy: row.toggleBusy
        interactive: false
        foreground: row.isCursor ? Color.menu.selectedText : root.foreground
        accent: Color.accent
        onToggled: row.activated()
      }
    }
  }
}
