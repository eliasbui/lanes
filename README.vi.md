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
  tệp đích tại vị trí của mình. Không có bước ghép chậm ở cuối.
- **Tiếp tục tải.** Tiến độ được lưu vài lần mỗi phút; sau khi mất điện, treo máy hay thoát app, download tiếp tục từ
  chỗ đã dừng. App hỏi máy chủ đúng tệp cũ theo ETag, nên tệp đã đổi trên máy chủ sẽ tải lại thay vì trộn lẫn.
- **Bắt download từ trình duyệt.** Tiện ích nhận download trình duyệt vừa tạo rồi gửi sang Lanes kèm Referer, Cookie,
  User-Agent của trang, đây là thứ giúp phần lớn liên kết bị lỗi "403 Forbidden" tải được. Dùng với Chrome, Edge,
  Brave và Cốc Cốc; có cả menu chuột phải "Tải bằng Lanes". Nếu Lanes chưa chạy, tiện ích tự mở app trong thanh
  trạng thái để download bắt đầu ngay.
- **Nút tải video.** Rê chuột lên một video, nút "Tải video" có biểu tượng Lanes hiện ngay dưới video như IDM (nằm trong góc dưới của video khi video chạm đáy cửa sổ); bấm vào sẽ thấy các bản Lanes tải được cho
  video đó theo độ phân giải (1080p, 720p, ...) kèm dung lượng hoặc dung lượng ước tính. Tệp video nguyên vẹn đi qua
  engine nhiều kết nối như thường; luồng HLS (`.m3u8`) được tải song song từng đoạn, kể cả mã hóa AES-128, tiếp tục
  được khi dừng, rồi ghép thành một tệp `.ts` hoặc `.mp4`. Chưa hỗ trợ: YouTube và các luồng DASH khác, phát trực
  tiếp, ghép tiếng nằm ở luồng riêng, và video có DRM (Lanes không bao giờ động tới).
- **net-merge (beta).** Dán nhiều liên kết mirror của cùng một file, Lanes chia các đoạn cho từng nguồn và gộp băng
  thông. Nguồn nào khác kích thước hoặc không cho tải theo đoạn sẽ bị bỏ; mỗi đoạn tự chuyển sang nguồn khác khi một
  nguồn lỗi. (net-merge của Lanes là gộp nhiều nguồn tải, không liên quan tới các công cụ NetMerge gộp đường mạng.)
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
trang, hoặc bấm "Tải tiện ích đã giải nén" và chọn nó. App làm mới thư mục mỗi khi cập nhật, nên trình duyệt nhận bản
tiện ích khớp với app ở lần mở sau. Mã tiện ích cố định nhờ khóa trong `extension/manifest.json`, nên Lanes nhận ra nó
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
  đoạn ghi theo vị trí, tệp đi kèm để resume, và thử lại khi lỗi.
- `src/hls.rs` — danh sách HLS: các độ phân giải của bản chính, các đoạn, khóa và đoạn byte của một bản, và giải mã
  AES-128.
- `src/store.rs` — danh sách tải trong SQLite.
- `src/browser.rs` — native host, hộp thư mà tiện ích thả việc vào, và tiện ích app mang theo.
- `src/update.rs` — tự cập nhật: đọc bản mới nhất, kiểm tra SHA-256, thay app tại chỗ, khởi động lại.
- `src/autostart.rs` — mở cùng máy tính (LaunchAgent trên macOS, khóa Run trên Windows).
- `src/main.rs` — cửa sổ, tray và phần nối giữa trang web với engine.
- `ui/index.html` — toàn bộ giao diện, một tệp.

## Dữ liệu và riêng tư

Danh sách tải và cài đặt nằm trong `~/Library/Application Support/Lanes` (macOS) hoặc `%APPDATA%\Lanes` (Windows).
Lanes chỉ kết nối tới máy chủ bạn tải và tới GitHub để kiểm tra cập nhật; tiện ích trình duyệt chỉ gửi việc tới app
trên máy, không gửi ra mạng.

## Giấy phép

MIT. Xem [LICENSE](LICENSE). Lanes là chương trình độc lập, không liên kết với bất kỳ trình quản lý tải xuống nào khác.
