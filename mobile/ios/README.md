# Mini Remote for iPhone and iPad

Mini Remote requires iOS or iPadOS 17 or later. The SwiftUI viewer uses Apple’s native H.264 playback pipeline. The macOS and Linux connectors use the same Rust service as the desktop viewer.

## Build and install

1. Open `mobile/ios/MiniRemote.xcodeproj` in Xcode on a Mac.
2. Choose your iPhone or iPad as the run destination. Under **Signing & Capabilities**, select your development team. Use a unique bundle identifier if your team can’t register the default one.
3. Run the `MiniRemote` scheme. Follow any device prompts for trust and Developer Mode.

If a local build shows **Untrusted Developer**, open **Settings → General → VPN & Device Management** on the iPhone, select the matching **Developer App** profile, and follow the trust prompt while online. A Personal Team build needs to be rebuilt and reinstalled when its provisioning profile expires. [Apple Developer account overview](https://developer.apple.com/help/account/basics/about-your-developer-account/)

TestFlight and the App Store require a team enrolled in the Apple Developer Program. Use that team and the registered bundle ID to archive in Xcode. For public review, prepare a privacy policy URL and a demo setup that reviewers can access independently. [Apple distribution guide](https://developer.apple.com/documentation/Xcode/distributing-your-app-for-beta-testing-and-releases/) [App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)

## Connect to a desktop

### Pair with a QR code

1. Update Mini Remote Viewer and Connector on the desktop you want to control. Connect the desktop and your iPhone or iPad to the same Tailscale network.
2. On that desktop, open Mini Remote Viewer and select **Setup → Pair an iPhone or iPad**.
3. On your iPhone or iPad, tap **Scan pairing code** and point the camera at the desktop. Check the desktop name and address, then tap **Pair & Connect**.

The code expires after three minutes and can be used once. Mini Remote stores the desktop credential in the device’s Keychain. If a code expires or was already used, select **Show a new code** on the desktop.

### Manual pairing

1. Run the latest Mini Remote Connector on the Mac or Linux host. Get the pairing token with `remote-connector pair`. On a Mac with the packaged connector, use:

   ```sh
   "$HOME/Applications/MiniRemote Connector.app/Contents/MacOS/remote-connector" pair
   ```

2. Connect Tailscale on the iPhone or iPad and on the host. Find the host’s Tailscale IPv4 address in Tailscale, or run `tailscale ip -4` on the host. Allow Mini Remote local network access if iOS asks and allow TCP 47990 in your tailnet policy if it restricts access.
3. Open Mini Remote and tap **Add device**. Enter a name, the numeric Tailscale IPv4 address, port `47990`, and the pairing token. Choose whether to remember the token in the device’s Keychain. Tap **Save & Connect**. When connecting to a saved device without a remembered token, enter the token and tap **Connect**.
4. Use **Pointer** to switch between direct touch and a virtual trackpad, right-click, or scroll. Use **View** to fit or fill the screen, or pinch to zoom and drag with two fingers to pan across the desktop. **Keyboard** opens text entry, modifiers, arrow keys, and clipboard actions. Use **Apps** to switch apps or close the active window. In landscape, the controls move to the right to give the desktop more vertical space.

For an ultrawide desktop, Mini Remote opens in **Center** view. Tap **Left** or **Right** to move across the display, or **Full** to see the whole desktop. If text looks blurry when zoomed, try **Sharp** in the saved device’s quality settings.

To move text between devices, enable **Share clipboard for this session** on connect and allow clipboard on the connector. In **Keyboard**, use the paste button to paste iPhone text into the focused desktop app. After copying text on the host during the session, tap **Copy from desktop** to put it on the iPhone clipboard.

The viewer connects through the Tailscale tunnel interface using a numeric IPv4 address. It requires an active tunnel with a single matching interface. Access to a locked desktop follows the host connector’s OS permissions and session restrictions described in the [desktop guide](../../README.md).

## Choose stream quality

| Setting | Maximum resolution | Frame rate | Video bitrate target |
| --- | --- | --- | --- |
| Low data | 854 × 480 | 15 fps | 850 kbps |
| Balanced | 1280 × 720 | 24 fps | 2.2 Mbps |
| Sharp | 1920 × 1080 | 30 fps | 4.8 Mbps |

**Automatic** starts on Balanced and uses Low data when iOS reports an expensive or constrained path. It can reconnect once at Low data if a Balanced connection stalls. The connector preserves the desktop aspect ratio. Actual throughput depends on desktop motion and encoder behavior and includes tunnel overhead.
