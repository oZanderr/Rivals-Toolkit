# Bundled bypass binaries

## version.dll

Oxide ASI Loader (`oxiloader`) by oZanderr, redistributed unmodified.
Source: https://github.com/oZanderr/oxiloader
Build: v0.2.1 release asset `version.dll`.
SHA-512: 655b6a09b5865fd289c9a7cdc1c15b3bad3191931866a0957bac1bd6a64484a9bd75423cba129058b88f0a07a64db2197fc7eb87180fc11313e7bce35b20a334

Licensed under the MIT License, Copyright (c) 2026 oZanderr. The full license
text is in `oxiloader-LICENSE.txt` alongside this file.

Installed next to the game executable as `version.dll`; it loads `.asi`
plugins from the `plugins` subfolder.

## RivalsSigBypass.asi

The pak signature bypass payload, built from oZanderr/rivals-sigbypass: a
plain-DllMain cdylib renamed from `rivals_sigbypass.dll`.
Source: https://github.com/oZanderr/rivals-sigbypass
Build: v0.1.1 release asset `RivalsSigBypass.asi`.
SHA-512: 1216a53bddc82f476cc27049fe477b804c903c58ad35132242074287888f644b172e80abe06e603e513cd84eda49c8ad9fb13d6c031fdea2a94d55113776617f
