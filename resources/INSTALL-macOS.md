# Installing Oryxis on macOS

Oryxis is signed, but not with an Apple Developer ID: that identity costs a yearly membership, and the app does not need one to run. macOS therefore treats it as coming from an unidentified developer and refuses the first launch. This file is how you get past that. You only do it once per install.

## From the .dmg

1. Open the `.dmg` and drag **Oryxis** onto the **Applications** shortcut beside it.
2. Eject the disk image, then open Oryxis from Applications.
3. The first launch is blocked. Depending on your macOS version you will see either a dialog with an **Open** button, or one that only offers to move the app to the Trash.
4. If there is no **Open** button, go to **System Settings > Privacy & Security**, scroll to the Security section, and click **Open Anyway** next to the message about Oryxis. Confirm with Touch ID or your login password.

If you prefer the terminal, this does the same thing in one step and works on every macOS version:

```
xattr -d com.apple.quarantine /Applications/Oryxis.app
```

## From the .tar.gz

The tarball holds the bare executable, with no app bundle. Use it when you want the binary itself, for a scripted install or to run it from the terminal.

```
tar xzf oryxis-macos-aarch64.tar.gz
chmod +x oryxis
xattr -d com.apple.quarantine oryxis
./oryxis
```

Running it this way is not the same as running the bundle: without `Oryxis.app` around it, macOS gives the process a different code identity, so anything keyed to that identity asks again. Touch ID unlock is the one you will notice, since the Keychain will want your login password the first time. Prefer the `.dmg` for daily use.

## From a nightly .bin

`oryxis-nightly-macos-aarch64.bin` is the same bare executable under a different name. The nightly build publishes it because the in-app updater replaces its own binary in place and needs the executable rather than an installer, so if you already run a nightly you do not have to download anything by hand.

To run one manually, treat it exactly like the tarball binary:

```
chmod +x oryxis-nightly-macos-aarch64.bin
xattr -d com.apple.quarantine oryxis-nightly-macos-aarch64.bin
./oryxis-nightly-macos-aarch64.bin
```

Do not copy it over the executable inside an installed `Oryxis.app`: that invalidates the bundle's signature. If you already did, re-sign it with `codesign -f -s - /Applications/Oryxis.app`.

## Verifying the download

Every release asset carries a detached Ed25519 signature (`<file>.sig`) next to it, and the app checks that signature on every update it installs. Verifying by hand is optional.

## Uninstalling

Drag `Oryxis.app` to the Trash. Your vault, keys and settings live in `~/.oryxis`, which is left alone; delete that folder too if you want nothing left behind.
