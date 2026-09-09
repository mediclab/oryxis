viewport: 1200x750
mode: Zen
-----
# J1: the download-mirror block in Settings > Advanced. Pick Custom,
# an http:// URL is rejected with the inline error, a valid https://
# URL commits and persists. The pick_list dropdown rows live in an
# overlay the text selector cannot see, hence the coordinate clicks
# (stable under the fixed viewport).
#
# Those coordinates are POSITIONAL, so adding an option to the picker
# moves them: `Project mirror` landed third and pushed Custom from
# y=226 to y=267, which silently retargeted the click and failed the
# whole run at the Save that no longer had a URL field above it. A new
# mirror mode means recalibrating this file in the same change. So
# does a row ABOVE the picker: the offline-mode toggle now leads the
# card and moved the whole block down by 72 px (picker 103 -> 175,
# Custom 267 -> 339, URL field 187 -> 259).
expect "Welcome to Oryxis"
click "Skip"
click "Continue without password"
expect "Create host"
click (1175, 64)
expect "Advanced"
click "Advanced"
expect "Download mirror"
click (1048, 175)
settle 300
click (1048, 339)
settle 300
click (410, 259)
type "http://not-safe.example"
click "Save"
expect "Enter a valid https:// URL"
click (410, 259)
type ctrl+a
type "https://mirror.example.cn"
click "Save"
settle 300
screenshot download-mirror-saved
expect "Download mirror"
