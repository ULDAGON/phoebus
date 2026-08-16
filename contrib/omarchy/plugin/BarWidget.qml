// Phoebus bar widget for the Omarchy shell.
//
// A Phoebus-only sibling of the built-in omarchy.media widget: it appears when
// Phoebus's MPRIS player is on the bus (i.e. the app is running) as a play glyph
// plus the current track. The bar part is deliberately inert — no play/pause, no
// skipping, no wheel. Its one interaction is a left click, which opens a
// now-playing panel: cover art, title/artist/album, a seek bar, and transport
// buttons. Everything else happens in the panel.
//
// Install to ~/.config/omarchy/plugins/phoebus.media/ and add
// {"id": "phoebus.media"} to a bar section of ~/.config/omarchy/shell.json.

import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Services.Mpris
import qs.Ui
import qs.Commons

Panel {
  id: root
  moduleName: "phoebus.media"
  ipcTarget: "phoebus.media"

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
  readonly property string album: player ? (player.trackAlbum || "") : ""
  readonly property string artUrl: player ? (player.trackArtUrl || "") : ""
  readonly property bool playing: player ? player.isPlaying : false
  readonly property string barLabel: title + (artist ? "  ·  " + artist : "")

  // Position/length in seconds. Quickshell computes `position` lazily; the
  // timer below re-emits its change signal once a second while the panel is
  // open so the readout and the slider actually advance.
  readonly property real trackLength: player && player.lengthSupported ? player.length : 0
  readonly property bool seekable: player !== null && player.canSeek && trackLength > 0

  property real maxLabelWidth: setting("maxLabelWidth", 180)

  // The up-next queue, from Phoebus's own org.phoebus.Queue service — MPRIS has
  // no queue. Refreshed while the panel is open; rows and the shuffle state
  // both come from one Upcoming call.
  property bool queueOpen: false
  property bool queueShuffle: false
  property var upcoming: []

  function refreshQueue() {
    if (!queueProc.running) queueProc.running = true
  }

  function applyQueue(text) {
    try {
      var data = JSON.parse(JSON.parse(text).data[0])
      queueShuffle = !!data.shuffle
      upcoming = data.upcoming || []
    } catch (e) {
      queueShuffle = false
      upcoming = []
    }
  }

  function queueCall(member, args) {
    var cmd = ["busctl", "--user", "call", "org.phoebus.Phoebus", "/org/phoebus", "org.phoebus.Queue", member]
    Quickshell.execDetached(args ? cmd.concat(args) : cmd)
    queueRefreshDelay.restart()
  }

  // Panel (unlike BarWidget) does not inject bar geometry, so read it off the bar.
  readonly property bool vertical: bar ? bar.vertical : false

  visible: player !== null
  implicitWidth: player ? row.implicitWidth + Style.space(14) : 0
  implicitHeight: bar ? bar.barSize : Style.space(24)

  function fmtTime(secs) {
    var s = Math.max(0, Math.round(secs))
    var h = Math.floor(s / 3600)
    var m = Math.floor((s % 3600) / 60)
    var r = s % 60
    var rr = (r < 10 ? "0" : "") + r
    if (h > 0) {
      var mm = (m < 10 ? "0" : "") + m
      return h + ":" + mm + ":" + rr
    }
    return m + ":" + rr
  }

  function commitSeek(v) {
    if (root.player && root.seekable) root.player.position = v
  }

  // ---- The bar part: a play glyph and the track, nothing else ----

  Row {
    id: row
    anchors.centerIn: parent
    spacing: Style.space(6)

    Text {
      anchors.verticalCenter: parent.verticalCenter
      text: "󰐊"
      color: root.bar ? root.bar.barForeground : "white"
      font.family: root.bar ? root.bar.fontFamily : Style.font.family
      font.pixelSize: Style.font.body
    }

    Text {
      anchors.verticalCenter: parent.verticalCenter
      visible: !root.vertical && root.barLabel !== ""
      text: root.barLabel
      elide: Text.ElideRight
      width: Math.min(implicitWidth, root.maxLabelWidth)
      color: root.bar ? root.bar.barForeground : "white"
      font.family: root.bar ? root.bar.fontFamily : Style.font.family
      font.pixelSize: Style.font.body
    }
  }

  // Left click toggles the panel. Only the left button is accepted, so a
  // right or middle click lands nowhere, and there is no wheel handler at
  // all — the bar surface stays a label, not a control.
  MouseArea {
    id: button
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    acceptedButtons: Qt.LeftButton
    onClicked: root.toggle()
    onEntered: if (root.bar) root.bar.showTooltip(root, root.barLabel !== "" ? root.barLabel : "Phoebus")
    onExited: if (root.bar) root.bar.hideTooltip(root)
  }

  // While the panel is open, keep the lazily-computed MPRIS position fresh.
  Timer {
    interval: 1000
    repeat: true
    triggeredOnStart: true
    running: root.opened && root.player !== null
    onTriggered: root.player.positionChanged()
  }

  // …and the queue, on a slower beat. Also refreshed shortly after any verb sent
  // via queueCall, so a jump or a shuffle toggle shows within a blink.
  Timer {
    interval: 2000
    repeat: true
    triggeredOnStart: true
    running: root.opened && root.player !== null
    onTriggered: root.refreshQueue()
  }

  Timer {
    id: queueRefreshDelay
    interval: 350
    onTriggered: root.refreshQueue()
  }

  Process {
    id: queueProc
    command: ["busctl", "--user", "--timeout=2", "call",
              "org.phoebus.Phoebus", "/org/phoebus", "org.phoebus.Queue",
              "Upcoming", "--json=short"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.applyQueue(text)
    }
  }

  // ---- The panel: cover, track, seek, transport ----

  KeyboardPanel {
    id: panel
    anchorItem: root
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(300))
    contentHeight: panel.fittedContentHeight(column.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onCloseRequested: root.close()
      onActivateRequested: if (root.player) root.player.togglePlaying()
      onMoveRequested: function(dx, dy) {
        if (dx !== 0 && root.seekable)
          root.commitSeek(Math.max(0, Math.min(root.trackLength, root.player.position + dx * 5)))
      }

      Column {
        id: column
        anchors.fill: parent
        spacing: Style.space(12)

        // Cover art, full width. The fallback note keeps the square from
        // collapsing when Phoebus has no artwork for the track (or nothing
        // is playing at all).
        BorderSurface {
          id: cover
          width: parent.width
          height: width
          radius: Style.spacing.labelGap
          color: Style.normalFillFor(root.bar.foreground, Color.accent)
          borderSpec: Border.controlSpec("normal", root.bar.foreground, Color.accent)

          Image {
            anchors.fill: parent
            anchors.margins: Style.space(2)
            fillMode: Image.PreserveAspectCrop
            asynchronous: true
            source: root.artUrl
            visible: root.artUrl !== ""
          }

          Text {
            anchors.centerIn: parent
            visible: root.artUrl === ""
            text: "󰝚"
            color: Qt.darker(root.bar.foreground, 1.4)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.space(64)
          }
        }

        // What is playing.
        Column {
          width: parent.width
          spacing: Style.space(3)

          Text {
            text: root.title || "Nothing playing"
            color: root.bar.foreground
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.subtitle
            font.bold: true
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
            width: parent.width
          }

          Text {
            text: root.artist
            color: Qt.darker(root.bar.foreground, 1.3)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.bodySmall
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
            width: parent.width
            visible: text !== ""
          }

          Text {
            text: root.album
            color: Qt.darker(root.bar.foreground, 1.6)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
            width: parent.width
            visible: text !== ""
          }
        }

        // The timestamp: elapsed on the left, total on the right, and a
        // slider that commits the seek on release (dragging previews the
        // time without spamming the player with intermediate seeks).
        Column {
          width: parent.width
          spacing: Style.space(2)
          visible: root.trackLength > 0

          Item {
            width: parent.width
            implicitHeight: elapsed.implicitHeight

            Text {
              id: elapsed
              text: root.fmtTime(seekSlider.dragging ? seekSlider.liveValue : (root.player ? root.player.position : 0))
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.caption
              anchors.left: parent.left
            }

            Text {
              text: root.fmtTime(root.trackLength)
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.caption
              anchors.right: parent.right
            }
          }

          PanelSlider {
            id: seekSlider
            bar: root.bar
            width: parent.width
            minimum: 0
            maximum: Math.max(1, root.trackLength)
            step: 5
            value: root.player ? root.player.position : 0
            enabled: root.seekable
            opacity: root.seekable ? 1.0 : 0.5

            onReleased: function(v) { root.commitSeek(v) }
          }
        }

        // Transport. The play/pause button is the biggest — it is the one
        // that matters — with previous/next flanking it, all on one centre
        // line (the taller play button would otherwise top-align the row).
        Row {
          anchors.horizontalCenter: parent.horizontalCenter
          spacing: Style.space(10)

          Button {
            anchors.verticalCenter: parent.verticalCenter
            iconText: "󰒮"
            foreground: root.bar.foreground
            horizontalPadding: Style.spacing.controlPaddingX
            verticalPadding: Style.spacing.controlPaddingY
            enabled: root.player && root.player.canGoPrevious
            opacity: enabled ? 1.0 : 0.4
            onClicked: root.player.previous()
          }

          Button {
            anchors.verticalCenter: parent.verticalCenter
            iconText: root.playing ? "󰏤" : "󰐊"
            foreground: root.bar.foreground
            horizontalPadding: Style.spacing.panelGap
            verticalPadding: Style.spacing.controlPaddingY
            iconSize: Style.font.iconLarge
            enabled: root.player && (root.player.canTogglePlaying || root.player.canPlay || root.player.canPause)
            opacity: enabled ? 1.0 : 0.4
            onClicked: root.player.togglePlaying()
          }

          Button {
            anchors.verticalCenter: parent.verticalCenter
            iconText: "󰒭"
            foreground: root.bar.foreground
            horizontalPadding: Style.spacing.controlPaddingX
            verticalPadding: Style.spacing.controlPaddingY
            enabled: root.player && root.player.canGoNext
            opacity: enabled ? 1.0 : 0.4
            onClicked: root.player.next()
          }
        }

        // Shuffle on the left, the queue expander on the right, a hairline
        // between them — the header of the collapsible up-next section.
        Item {
          width: parent.width
          implicitHeight: Math.max(shuffleBtn.implicitHeight, expandBtn.implicitHeight)

          Button {
            id: shuffleBtn
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            iconText: "󰒝"
            tooltipText: "Shuffle"
            active: root.queueShuffle
            foreground: root.queueShuffle ? Color.accent : root.bar.foreground
            horizontalPadding: Style.spacing.controlPaddingX
            verticalPadding: Style.spacing.controlPaddingY
            onClicked: root.queueCall("ToggleShuffle")
          }

          Rectangle {
            anchors.left: shuffleBtn.right
            anchors.right: expandBtn.left
            anchors.leftMargin: Style.space(10)
            anchors.rightMargin: Style.space(10)
            anchors.verticalCenter: parent.verticalCenter
            height: 1
            color: Util.alpha(root.bar.foreground, 0.18)
          }

          Button {
            id: expandBtn
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            iconText: root.queueOpen ? "󰅀" : "󰅂"
            tooltipText: "Up next"
            foreground: root.bar.foreground
            horizontalPadding: Style.spacing.controlPaddingX
            verticalPadding: Style.spacing.controlPaddingY
            onClicked: root.queueOpen = !root.queueOpen
          }
        }

        // The queue itself: coverless one-line rows, click to play that song.
        Column {
          width: parent.width
          spacing: Style.space(2)
          visible: root.queueOpen

          Text {
            visible: root.upcoming.length === 0
            text: "Nothing queued"
            color: Qt.darker(root.bar.foreground, 1.6)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            horizontalAlignment: Text.AlignHCenter
            width: parent.width
          }

          ListView {
            width: parent.width
            height: Math.min(contentHeight, Style.space(230))
            clip: true
            interactive: contentHeight > height
            model: root.upcoming
            visible: root.upcoming.length > 0

            delegate: Rectangle {
              id: upRow
              required property var modelData
              required property int index

              width: ListView.view.width
              height: Style.space(26)
              radius: Style.spacing.labelGap
              color: rowMouse.containsMouse
                ? Style.hoverFillFor(root.bar.foreground, Color.accent)
                : "transparent"

              Text {
                id: upTitle
                anchors.left: parent.left
                anchors.leftMargin: Style.space(6)
                anchors.right: upArtist.left
                anchors.rightMargin: Style.space(8)
                anchors.verticalCenter: parent.verticalCenter
                text: upRow.modelData.title || "Unknown"
                color: root.bar.foreground
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.bodySmall
                elide: Text.ElideRight
              }

              Text {
                id: upArtist
                anchors.right: parent.right
                anchors.rightMargin: Style.space(6)
                anchors.verticalCenter: parent.verticalCenter
                width: Math.min(implicitWidth, upRow.width * 0.4)
                horizontalAlignment: Text.AlignRight
                text: upRow.modelData.artist || ""
                color: Qt.darker(root.bar.foreground, 1.5)
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.caption
                elide: Text.ElideRight
              }

              MouseArea {
                id: rowMouse
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.queueCall("Jump", ["u", String(upRow.index)])
              }
            }
          }
        }
      }
    }
  }
}
