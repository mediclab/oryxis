viewport: 1200x750
mode: Zen
-----
# Offline mode: offered on the first-run features step (its row leads
# the list, above the fold), persisted before a vault exists, reported
# where a silenced fetch would have acted (Settings > About, the
# Plugins panel), superseding the download mirror in Settings >
# Advanced, and surviving a restart. Toggle switches are hit by
# position: a toggle row's label is not a button.
expect "Welcome to Oryxis"
click "Next"
click "Next"
click "Next"
click "Next"
settle
expect "Make it yours"
expect "Offline mode"
click (810, 358)
settle
click "Next"
settle
click "Next"
settle
expect "Protect your vault"
click "Continue without password"
settle
expect "Create host"
click (1175, 64)
expect "Advanced"
click "Advanced"
settle
expect "Offline mode"
# The mirror block is not built while the switch is on; one line says why.
expect "The download mirror does not apply while offline mode is on."
absent "Download mirror"
click "About"
settle
# Said whether or not a check was asked for.
expect "Update checks are off: offline mode is on. Turn it off in Settings > Advanced to check again."
click "Check for updates"
settle
expect "Update checks are off: offline mode is on. Turn it off in Settings > Advanced to check again."
click "Features & Plugins"
settle
expect "Offline mode is on: the plugin catalog and downloads are paused. Installed plugins keep working."
# A second session reads the switch back from the settings table.
reset
settle
expect "Create host"
click (1175, 64)
expect "Advanced"
click "Advanced"
settle
expect "The download mirror does not apply while offline mode is on."
# Off gives the mirror block back.
click (1142, 94)
settle
expect "Download mirror"
absent "The download mirror does not apply while offline mode is on."
screenshot offline-mode-off
