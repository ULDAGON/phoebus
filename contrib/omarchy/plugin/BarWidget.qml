// Phoebus bar widget for the Omarchy shell.
//
// A Phoebus-only sibling of the built-in omarchy.media widget: it appears when
// Phoebus's MPRIS player is on the bus (i.e. the app is running), shows the track,
// and drives playback. Install to ~/.config/omarchy/plugins/phoebus.media/ and add
// {"id": "phoebus.media"} to a bar section of ~/.config/omarchy/shell.json.
//
// Interactions: left click play/pause, right click next track, middle click raises
// the Phoebus window, wheel steps previous/next.

import QtQuick
import Quickshell
import Quickshell.Services.Mpris
import qs.Ui
import qs.Commons

BarWidget {
  id: root
  moduleName: "phoebus.media"

  readonly property var players: Mpris.players ? Mpris.players.values : []
  readonly property var player: {
    for (var i = 0; i < players.length; i++) {
      var p = players[i]
      if (!p) continue
      if (String(p.identity || "") === "Phoebus") return p
      if (String(p.desktopEntry || "").toLowerCase() === "phoebus") return p
    }
    return null
  }

  readonly property string title: player ? (player.trackTitle || "") : ""
  readonly property string artist: player ? (player.trackArtist || "") : ""
  readonly property string label: title + (artist ? "  ·  " + artist : "")
  readonly property string playIcon: player && player.isPlaying ? "󰏤" : "󰐊"

  property real maxLabelWidth: 180

  visible: player !== null
  implicitWidth: player ? row.implicitWidth + Style.space(14) : 0
  implicitHeight: barSize

  Row {
    id: row
    anchors.centerIn: parent
    spacing: Style.space(6)

    // The wordmark dot: what marks this widget as Phoebus's rather than generic media.
    Text {
      anchors.verticalCenter: parent.verticalCenter
      text: "●"
      color: root.bar.barForeground
      font.family: root.bar.fontFamily
      font.pixelSize: Style.font.body
    }

    Text {
      anchors.verticalCenter: parent.verticalCenter
      text: root.playIcon
      color: root.player && root.player.isPlaying
        ? root.bar.barForeground
        : Qt.darker(root.bar.barForeground, 1.5)
      font.family: root.bar.fontFamily
      font.pixelSize: Style.font.body
    }

    Text {
      anchors.verticalCenter: parent.verticalCenter
      visible: !root.vertical && root.label !== ""
      text: root.label
      elide: Text.ElideRight
      width: Math.min(implicitWidth, root.maxLabelWidth)
      color: root.bar.barForeground
      font.family: root.bar.fontFamily
      font.pixelSize: Style.font.body
    }
  }

  MouseArea {
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: root.player ? Qt.PointingHandCursor : Qt.ArrowCursor
    acceptedButtons: Qt.LeftButton | Qt.RightButton | Qt.MiddleButton

    onClicked: function(mouse) {
      if (!root.player) return
      if (mouse.button === Qt.MiddleButton) {
        if (root.player.canRaise) root.player.raise()
      } else if (mouse.button === Qt.RightButton) {
        if (root.player.canGoNext) root.player.next()
      } else {
        root.player.togglePlaying()
      }
    }
    onWheel: function(wheel) {
      if (!root.player) return
      if (wheel.angleDelta.y > 0 && root.player.canGoPrevious) root.player.previous()
      else if (wheel.angleDelta.y < 0 && root.player.canGoNext) root.player.next()
    }
    onEntered: if (root.bar) root.bar.showTooltip(root, root.label !== "" ? root.label : "Phoebus")
    onExited: if (root.bar) root.bar.hideTooltip(root)
  }
}
