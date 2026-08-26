# Bundled bypass binaries

## dsound.dll

Oxide ASI Loader (`oxiloader`) by oZanderr, redistributed unmodified.
Source: https://github.com/oZanderr/oxiloader
Build: v0.2.1 release asset `dsound.dll`.
SHA-512: 263815e9332d01d824cb0bd872c04d6c33a6a8d2af81ace1d07d10170649ae1cc0813873f180f32954e5380ff9c9ff120b7d54e81e90527d395713c43f491bf3

Licensed under the MIT License, Copyright (c) 2026 oZanderr. The full license
text is in `oxiloader-LICENSE.txt` alongside this file.

Installed next to the game executable as `dsound.dll`; it loads `.asi`
plugins from the `plugins` subfolder. The same release builds the loader under
other proxy names as well, including the `version.dll` that earlier toolkit
versions installed.

## MarvelRivalsUTOCSignatureBypass.asi

The community pak/utoc signature bypass payload, redistributed unmodified from
its original Nexus Mods release. No license text accompanies the binary.
SHA-512: 73218a0f4762781830c6e289fdcc0a64d6b59bc2381b2ac29bcdbae124149dd4206bb10c2bfa8f209282fd1e7d3808e2eb0e0f0295bf95050deff67ab7e5abcc
Size: 44,032 bytes.

Installed into the `plugins` subfolder next to the loader.

Ship this exact build. The toolkit previously installed its own payload
(oZanderr/rivals-sigbypass, `RivalsSigBypass.asi`) and the game began flagging
it, while this original binary keeps loading; a rebuild is not a substitute.
