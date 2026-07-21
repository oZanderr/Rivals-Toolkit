# Bundled bypass binaries

## version.dll

Ultimate ASI Loader by ThirteenAG, redistributed unmodified.
Source: https://github.com/ThirteenAG/Ultimate-ASI-Loader
Build: x64-latest release asset `version-x64.zip`.
SHA-512: 8a9df89d57115ca00e6aa97d5d7071adf64494282c21a7a59dc6f09baa097758968bf77d217dd181b94f21c9ce0f4881258c35509cb63e1a2bc729deb1ac8dab

Licensed under the MIT License, Copyright (c) 2023 ThirteenAG. The full license
text and copyright notice are in `Ultimate-ASI-Loader-LICENSE.txt` alongside
this file, as MIT requires when redistributing the binary.

Installed next to the game executable as `version.dll`; it loads `.asi`
plugins from the `plugins` subfolder.

## RivalsSigBypass.asi

The pak signature bypass payload, built from oZanderr/rivals-sigbypass
(`main` branch): a plain-DllMain cdylib renamed from `rivals_sigbypass.dll`.
Source: https://github.com/oZanderr/rivals-sigbypass
