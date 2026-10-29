<p align="center">
  <img src="assets/icon.png" width="112" alt="" />
</p>

<h1 align="center">Lanes</h1>

<p align="center">
  Trình quản lý tải xuống nhanh, mã nguồn mở cho macOS và Windows.<br />
  Nhiều kết nối mỗi tệp, tiếp tục sau khi dừng, bắt download từ trình duyệt, trong cửa sổ Liquid&nbsp;Glass.
</p>

<p align="center">
  <a href="README.md"><img src="https://img.shields.io/badge/English-grey?style=for-the-badge" alt="English" /></a>
  <a href="README.vi.md"><img src="https://img.shields.io/badge/Ti%E1%BA%BFng%20Vi%E1%BB%87t-blue?style=for-the-badge" alt="Tiếng Việt (hiện tại)" /></a>
</p>

<p align="center">
  <a href="https://github.com/eliasbui/lanes/releases/latest"><img src="https://img.shields.io/github/v/release/eliasbui/lanes?label=release" alt="Bản mới nhất" /></a>
  <img src="https://img.shields.io/badge/macOS-13%2B-black" alt="macOS 13 trở lên" />
  <img src="https://img.shields.io/badge/Windows-10%2B-black" alt="Windows 10 trở lên" />
  <a href="LICENSE"><img src="https://img.shields.io/badge/gi%E1%BA%A5y%20ph%C3%A9p-MIT-blue" alt="Giấy phép MIT" /></a>
</p>

Lanes là trình quản lý tải xuống viết bằng Rust. Nó mở nhiều kết nối tới một tệp cùng lúc, ghi thẳng mỗi đoạn vào đúng
vị trí trong tệp đích, và lưu một tệp đi kèm để tải dở tiếp tục được thay vì tải lại từ đầu. Tiện ích trình duyệt chuyển
download sang app kèm header của trang, nên máy chủ đã phục vụ tệp cho trình duyệt cũng phục vụ cho Lanes, và gắn nút
tải lên các video bạn xem, có chọn độ phân giải. Cửa sổ là một
trang web trên nền kính; mọi quy tắc phía sau là Rust có kiểm thử. App chạy trong thanh trạng thái, có tiếng Anh và
tiếng Việt.

## Tính năng

- **Nhiều kết nối mỗi tệp.** Mỗi tệp được chia thành các đoạn bằng nhau, tối đa 32 kết nối, mỗi kết nối ghi thẳng vào
  tệp đích tại vị trí của mình. Không có bước ghép chậm ở cuối. Kết nối nào xong đoạn của mình thì nhận nửa sau của
  đoạn còn lại nhiều nhất, như IDM, nên một kết nối chậm không giữ chân phần cuối của tệp.
- **Tiếp tục tải.** Tiến độ được lưu vài lần mỗi phút; sau khi mất điện, treo máy hay thoát app, download tiếp tục từ
  chỗ đã dừng. App hỏi máy chủ đúng tệp cũ theo ETag, nên tệp đã đổi trên máy chủ sẽ tải lại thay vì trộn lẫn.
- **Bắt download từ trình duyệt.** Tiện ích nhận download trình duyệt vừa tạo rồi gửi sang Lanes kèm Referer, Cookie,
  User-Agent của trang, đây là thứ giúp phần lớn liên kết bị lỗi "403 Forbidden" tải được. Dùng với Chrome, Edge,
  Brave và Cốc Cốc; có cả menu chuột phải "Tải bằng Lanes". Nếu Lanes chưa chạy, tiện ích tự mở app trong thanh
  trạng thái để download bắt đầu ngay.
- **Nút tải video.** Nút tròn của Lanes nằm ở góc trên bên phải video khi rê chuột lên, và hiện vài giây khi video bắt
  đầu phát; bấm vào sẽ thấy các bản Lanes tải được cho video đó theo độ phân giải (4K, 2K, 1080p, ...) kèm dung lượng
  hoặc dung lượng ước tính. Tệp video nguyên vẹn đi qua
  engine nhiều kết nối như thường; luồng HLS (`.m3u8`) được tải song song từng đoạn, kể cả mã hóa AES-128, tiếp tục
  được khi dừng, rồi ghép thành một tệp `.ts` hoặc `.mp4`. YouTube và các trang Lanes không tự tìm ra luồng (trang
  DASH) đi qua yt-dlp: menu liệt kê mọi độ phân giải tới 2160p và bản chỉ âm thanh, bản được chọn tải về và ghép thành
  MP4. Không hỗ trợ: phát trực tiếp, và video có DRM (Lanes không bao giờ động tới).
- **Tiến độ ngay trên trình duyệt.** Những gì Lanes đang tải cũng hiện trên trình duyệt: nút tiện ích trên thanh công cụ
  ghi phần trăm đã tải, khay ở góc trang đang xem có một vòng tiến độ cho mỗi download và đổi thành dấu tích khi xong
  (kèm nút mở thư mục chứa file), cửa sổ nhỏ của tiện ích liệt kê các download gần nhất.
- **yt-dlp** cần cho YouTube và các trang DASH; Lanes tự tìm ở chỗ Homebrew hoặc winget cài. macOS:
  `brew install yt-dlp`. Windows: `winget install yt-dlp.yt-dlp DenoLand.Deno`. ffmpeg để ghép hình với tiếng (720p
  đến 4K) đã có sẵn trong Lanes: bản FFmpeg 4 MB giấy phép LGPL, chỉ ghép luồng, build từ mã nguồn FFmpeg bằng
  `scripts/build-ffmpeg.sh`.
- **net-merge: mọi đường mạng cùng lúc.** Bật công tắc trong Cài đặt, các kết nối của một lượt tải được chia ra mọi
  đường mạng của máy (Wi-Fi, Ethernet, điện thoại cắm USB bật điểm truy cập), như
  [net-merge](https://github.com/net-merge/net-merge), nên một tệp được tải bằng tổng tốc độ của các đường: tệp thường
  theo từng đoạn, HLS theo từng phần, trang qua yt-dlp tám phần một lúc. Không cần quyền quản trị: chỉ kết nối của
  Lanes đi qua một proxy của Lanes trên 127.0.0.1, proxy đòi một mật khẩu tạo ngẫu nhiên, và mỗi kết nối ra khỏi máy
  bằng đường được giao; ngoài ra không thay đổi gì trên máy. Kết nối đầu tiên của một lượt tải tới mỗi trang đi đường
  mạng thường dùng của hệ thống, một đường khác chỉ nhận một trang sau khi curl của hệ thống đã tới được trang đó qua
  đường ấy với chứng chỉ nó chấp nhận (mạng có tường lửa thay chứng chỉ của vài trang bị phát hiện ở bước này), đường
  nào làm hỏng kết nối với một trang thì tránh trang đó một thời gian, và đường không ra được internet thì không nhận
  gì. YouTube ký địa chỉ video theo một địa chỉ công khai, nên các phần của nó ở trên một đường. Khi VPN chở toàn bộ
  lưu lượng thì không gộp gì. Cài đặt liệt kê các đường mạng cùng lưu lượng của từng đường.
- **net-merge (beta): nhiều mirror.** Dán nhiều liên kết mirror của cùng một file, Lanes chia các đoạn cho từng nguồn
  và gộp băng thông. Nguồn nào khác kích thước hoặc không cho tải theo đoạn sẽ bị bỏ; mỗi đoạn tự chuyển sang nguồn
  khác khi một nguồn lỗi.
- **Tạm dừng, tiếp tục, tốc độ và thời gian còn lại** cho từng tệp, theo thời gian thực.
- **Phân loại như IDM.** Tệp được lưu vào `Compressed`, `Documents`, `Music`, `Programs`, `Video` trong thư mục lưu
  theo đuôi tệp (danh sách của IDM, thêm dmg và pkg vào chương trình); tệp khác để ngay ở thư mục lưu. Tắt được bằng
  một công tắc trong Cài đặt.
- **Chỉnh số kết nối**, mở cùng máy tính, và tự cập nhật trong app.
- **Giao diện Liquid Glass**, sáng và tối, theo hệ thống.

## Cài đặt

### macOS

Máy Mac chip Apple hoặc Intel, macOS 13 trở lên. Dùng Homebrew:

    brew tap eliasbui/lanes
    brew install --cask eliasbui/lanes/lanes

Không dùng Homebrew:

    curl -fsSL https://raw.githubusercontent.com/eliasbui/lanes/main/scripts/install.sh | sh

Ghi đủ tên cask như trên thì Homebrew chỉ tin đúng cask này, không cần `brew trust`. App chỉ ký ad-hoc và chưa
notarize, nên cả hai cách đều gỡ cờ quarantine sau khi cài để macOS cho mở. `brew uninstall --cask lanes` gỡ app, thêm
`--zap` để xóa cả danh sách tải trong `~/Library/Application Support/Lanes`.

### Windows

Mở PowerShell và chạy:

    irm https://raw.githubusercontent.com/eliasbui/lanes/main/scripts/install.ps1 | iex

Script cài cho tài khoản đang dùng vào `%LOCALAPPDATA%\Programs\Lanes`, kiểm tra file tải về bằng SHA-256 mà GitHub
công bố, rồi thêm lối tắt vào menu Start. Chạy lại lệnh để cập nhật; gỡ trong Settings > Apps.

### Tiện ích trình duyệt

App mang sẵn tiện ích và ghi nó ra `~/Library/Application Support/Lanes/extension` (macOS) hoặc
`%APPDATA%\Lanes\extension` (Windows) mỗi lần mở; mục Cài đặt, Tiện ích trình duyệt cho biết thư mục này. Mở
`chrome://extensions` (hoặc trang tương ứng của Edge, Brave, Cốc Cốc), bật Chế độ nhà phát triển rồi kéo thư mục đó vào
trang, hoặc bấm "Tải tiện ích đã giải nén" và chọn nó. App làm mới thư mục mỗi khi cập nhật, và tiện ích tự tải lại từ
thư mục đó ngay khi thấy app có phiên bản mới hơn, không cần mở lại trình duyệt. Mã tiện ích cố định nhờ khóa trong `extension/manifest.json`, nên Lanes nhận ra nó
dù bạn tải từ đâu. Cửa sổ nhỏ của tiện ích cho biết đã kết nối được với app chưa, và bật tắt việc tự nhận download cùng
nút tải video.

## Xây dựng và chạy

Rust 1.85 trở lên.

    cargo run

    cargo test                       # engine, store, bộ cập nhật và native host
    cargo run --example make_icons   # vẽ lại icon trong assets/
    scripts/bundle-macos.sh          # dựng dist/Lanes.app cho máy này

## Cách hoạt động

- `src/engine.rs` — engine tải: thăm dò kích thước và khả năng tải theo đoạn, chia đoạn bằng nhau, mỗi kết nối một
  đoạn ghi theo vị trí, kết nối xong việc nhận nửa đoạn còn lại nhiều nhất, tệp đi kèm để resume, và thử lại khi lỗi.
- `src/netmerge.rs` — net-merge: các đường mạng của máy, kết nối gắn vào một đường (`IP_BOUND_IF` trên macOS, địa chỉ
  nguồn trên Windows), proxy SOCKS5, trọng số theo tốc độ đo được, thăm dò internet và kiểm tra một đường với một
  trang. Cùng một tệp với `crates/core-netmerge/src/lib.rs` của figghost.
- `src/hls.rs` — danh sách HLS: các độ phân giải của bản chính, các đoạn, khóa và đoạn byte của một bản, và giải mã
  AES-128.
- `src/ytdlp.rs` — trang video qua yt-dlp: các định dạng thành lựa chọn trong menu, và việc tải cùng tiến độ.
- `src/store.rs` — danh sách tải trong SQLite.
- `src/browser.rs` — native host, hộp thư mà tiện ích thả việc vào, và tiện ích app mang theo.
- `src/update.rs` — tự cập nhật: đọc bản mới nhất, kiểm tra SHA-256, thay app tại chỗ, khởi động lại.
- `src/autostart.rs` — mở cùng máy tính (LaunchAgent trên macOS, khóa Run trên Windows).
- `src/main.rs` — cửa sổ, tray và phần nối giữa trang web với engine.
- `ui/index.html` — toàn bộ giao diện, một tệp.

## Dữ liệu và riêng tư

Danh sách tải và cài đặt nằm trong `~/Library/Application Support/Lanes` (macOS) hoặc `%APPDATA%\Lanes` (Windows).
Lanes chỉ kết nối tới máy chủ bạn tải và tới GitHub để kiểm tra cập nhật; tiện ích trình duyệt chỉ gửi việc tới app
trên máy, không gửi ra mạng. Khi bật net-merge, và chỉ trong lúc có lượt tải, Lanes thỉnh thoảng mở một kết nối TCP
tới 1.1.1.1 và 8.8.8.8 cổng 443 qua từng đường mạng, không gửi gì, để biết đường nào ra được internet, và chạy curl
của hệ thống một lần cho mỗi cặp trang và đường mạng để hỏi phần đầu của trang đó qua đường ấy.

## Giấy phép

MIT. Xem [LICENSE](LICENSE). Lanes là chương trình độc lập, không liên kết với bất kỳ trình quản lý tải xuống nào khác.

`src/netmerge.rs` là bộ lập lịch của [net-merge](https://github.com/net-merge/net-merge) viết lại bằng Rust, bản
quyền 2026 Rohit Mahto, theo giấy phép MIT: xem [NET-MERGE-LICENSE.txt](NET-MERGE-LICENSE.txt).
