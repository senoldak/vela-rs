# Vela (Rust)

[LuxAlgo/Vela](https://github.com/LuxAlgo/Vela) finansal grafik kütüphanesinin Rust'a çevirisi. Özgün proje TypeScript ile yazılmıştır ve tarayıcıda çalışır. Çeviri iki katmandır: platformdan bağımsız bir çekirdek ve onu WebAssembly ile tarayıcıya bağlayan bir katman. Tarayıcıda tek grafik, çoklu grafik çalışma alanı ve tam pencere öğesi (üst çubuk, sembol ve gösterge seçiciler, kenar çubuğu, nesne ağacı, alt çubuk) çalışır; çizim Canvas 2D veya WebGL2 ile yapılır (`demo` klasörü). Özgün arayüz kiti çevrilmedi; arayüz parçaları aynı yapı ve davranışla yerel HTML öğeleriyle kuruldu. Betik motoru (Pine) yoktur.

Bu depo resmî olmayan bir çeviridir; LuxAlgo ile bir bağlantısı yoktur.

Çevirinin dayandığı özgün sürüm: `fc3e2a011834452db766bc154ef0549a1583c66b` (12 Kasım 2026).

## Durum

Çalışma alanında üç crate var:

- `crates/vela-core`: platformdan bağımsız çekirdek. Yerel hedefte ve `wasm32-unknown-unknown` hedefinde derlenir, JavaScript'e bağlanmaz. Dış bağımlılıkları `serde` ve `serde_json`.
- `crates/vela-web`: tarayıcı katmanı. Çekirdeğin ev sahibine bıraktığı hizmetleri `wasm-bindgen` üzerinden tarayıcıya bağlar: `Intl` API'si (`BrowserIntl`), zamanlayıcılar, `fetch`, WebSocket ve saat (`BrowserPlatform`), 2B tuval ve WebGL2 bağlamları (`BrowserCanvas`, `BrowserGl`), çizicinin tuval yığını ve DOM öğeleri (`BrowserHost`), fare, dokunma ve klavye bağlantısı (`InputWiring`), arayüz parçaları (`chrome_ui`, `dialogs`, `drawings_ui`, `shell_top`, `shell_side`, `workspace_ui`). JavaScript'e açtığı sınıflar: özgün API'nin karşılığı `Vela`, çoklu grafik `VelaWorkspace`, tam pencere öğesi `VelaWidget`, ve iki deneme yüzeyi (`VelaChart`, `VelaApp`). Sayfa düzeyindeki kayıt işlevleri: `registerLocale`, `registerTheme`, `registerWidgetAction`, `registerSidePanel`, `registerWidgetAttachment`, `registerLegendAction`, `registerLegendCallout`, `registerSymbolRanking`, `registerStatePersistence`, `registerShellDefaults`.
- `crates/xtask`: derleme görevleri. `vela-web`'i tarayıcıya hazır bir WebAssembly paketine dönüştürür.

`vela-core` modülleri:

| Modül | Özgün kaynak | İçerik |
|---|---|---|
| `model` | `src/core/model` | OHLCV çubukları, seri tipleri, sahne, girdi şeması, çizim nesneleri, gösterge modeli, yamalar, işlemler |
| `indicators` | `src/core/native-indicators` | Yerel gösterge yaşam döngüsü, çıktı tipi, kayıt defteri; hacim ve VPVR göstergeleri |
| `indicators::classics` | `src/core/native-indicators/classics` | Dizi matematiği, ortak yardımcılar ve klasik gösterge kataloğu |
| `drawings` | `src/core/drawings` | `Drawing` sözleşmesi, ortak durum, JSON kaydı, stil, ayar şeması, zaman dilimi görünürlüğü, seviye paleti, belge, depo, geri al/yinele geçmişi, araç varsayılanları ve şablonları, isabet testi matematiği |
| `drawings::types` | `src/core/drawings/types`, `registry.ts` | 79 çizim aracı ve kayıt defteri girdileri: çizgiler, şekiller, açıklamalar, kanallar ve pitchfork'lar, Fibonacci, Gann, Mach, formasyonlar, fiyat hareketi, ölçüm, istatistik |
| `chart_types`, `price_styles` | `src/chart-types`, `src/core/price-styles` | Grafik tipi kayıt defteri, ayar satırı tanımları, uygulanabilirlik kuralları, çubuk dönüşümü arayüzü, Heikin Ashi, genişletilmiş sembol ayrıştırma |
| `vela` | `src/Vela.ts` | `Vela` giriş sınıfı: seçenekleri çözer, motoru ve denetim yüzeylerini kurar; kayıt defterlerini taşıyan `VelaEnv` ortamı |
| `engine` | `src/core/engine` | Motor orkestratörü (piyasa yükleme ve değiştirme, canlı akış, boşluk onarma, göstergeler, paneller, tekrar oynatma), gösterge kayıt defteri ve tutamaçları, sahne incelemesi, çizim serisi servisi |
| `controls` | `src/core/*Control.ts` | `chart.data`, `chart.panes`, `chart.replay`, `chart.renderer`, `chart.drawings`, `chart.marks` yüzeyleri |
| `options`, `theme`, `renderer_defaults`, `script_run` | `src/core/options.ts`, `theme.ts`, `renderer-defaults.ts`, `script-run.ts` | Grafik seçenekleri, animasyon çözümleme, koyu ve açık temalar, tema kayıt defteri, betik çalıştırma sonucu |
| `runtime` | | Çekirdeğin eşzamansız soyutlaması: `Platform` (zamanlayıcı, `fetch`, WebSocket, saat, rastgele sayı), iptal sinyali, tek seferlik kanal; testler için elle sürülen `ManualPlatform` |
| `data` | `src/data` | Zaman dilimi süreleri, çubuk deposu, sembol yardımcıları, sağlayıcı kayıt defteri, çok sağlayıcılı besleme, önbellekli besleme |
| `data::providers` | `src/data/providers` | Binance, Coinbase ve Hyperliquid sağlayıcıları: REST geçmişi, sembol listeleri, WebSocket canlı akışı, yeniden bağlanma ve yoklamaya düşme, istek sınırlayıcılar |
| `ports` | `src/core/ports`, `src/core/drawings/port.ts` | Veri sağlayıcı, piyasa beslemesi, grafik çizici ve betik motoru arayüzleri; çizim niyetleri |
| `marks`, `events` | `src/core/marks`, `src/core/events` | Zaman çizelgesi işaretleri, grup görünürlüğü, çizimden bağımsız işaret durumu, tipli olay veri yolu, 46 grafik olayının adları ve yükleri |
| `state` | `src/state/document.ts` | Kalıcı çalışma alanı belgesi: tipler, kodlama, çözme, güvenilmeyen girdiyi temizleme |
| `visible_range`, `data_unavailable` | `src/core/visible-range.ts`, `data-unavailable.ts` | Görünür aralık ön ayarları, veri yok hatası |
| `util` | | JavaScript'in `toFixed` ve `String(n)` çıktısını veren sayı biçimleyiciler |
| `i18n` | `src/core/i18n` | Mesaj tanımlayıcıları, ICU mesaj biçimleyici (plural, selectordinal, select, number), dil etiketi çözümleme ve yedek zincirleri, dil paketi kayıt defteri, çevirmen |
| `format`, `timezones` | `src/core/format`, `timezones.ts` | Fiyat, yüzde, kısa sayı, eksen etiketi, zaman damgası, süre ve saat biçimleyicileri; takvim adları; saat dilimi listesi ve ofsetleri |
| `locales` | `src/locales` | 20 dil paketi (gömülü JSON, istendiğinde ayrıştırılır) |
| `intl` | | Çekirdeğin `Intl` soyutlaması ve yalnızca İngilizce bilen yerleşik uygulaması |
| `palette` | `packages/vela-ui/src/lib/palette.ts` | Anlamsal renk sabitleri |
| `render::canvas` | | Çizicilerin kullandığı 2B tuval soyutlaması (`Canvas2d`) ve her çağrıyı kaydeden `RecordingCanvas` |
| `render::core` | `src/renderers/native/core` | Koordinat sistemi, görünüm, otomatik ve elle ölçekleme, panel boyutlandırma, katman sırası, kare zamanlayıcı, animasyon, sahne grafiği, grafik yapılandırması, girdi denetleyicilerinin karar mantığı |
| `render::backend` | `src/renderers/native/backend` | Çizim arka ucu arayüzü, mum ayrıntı düzeyi, kapanış stilleri, katman kayıt defteri, hacim profili; Canvas 2D arka ucu ve WebGL2 arka ucu (mumlar, çubuklar, kapanış stilleri, gösterge serileri, dolgular, parlama geçişi) |
| `render::gl` | | WebGL2 arka ucunun kullandığı GL soyutlaması (`Gl`) ve her çağrıyı kaydeden `RecordingGl` |
| `render::chrome` | `src/renderers/native/chrome` | Eksen çentikleri ve etiketleri, sağ kenar yerleşimi, geri sayım, işaret şeridi yerleşimi; eksen, artı imleci ve işaret şeridi çizicileri |
| `render::backdrop`, `render::volume`, `render::vpvr` | `src/renderers/native/backdrop`, `volume`, `vpvr` | Izgara ve seans bantları; hacim ve hacim profili katmanları |
| `render::drawings` | `src/renderers/native/drawings` | 79 çizim aracının boyayıcısı, çizicinin projektörü, isabet testi, klavye kısayolu kararları, ölçüm cetveli, gösterge çizimi dilimleri; yerleştirme, seçim ve sürükleme durum makinesi (`DrawingInteraction`); çizici tarafındaki çizim denetleyicisi (`UserDrawingController`) |
| `render::native` | `src/renderers/native/NativeRenderer.ts` | Çizicinin kabuğu: sahne ve koordinat sistemi, panel yerleşimi, ölçek hesabı, kaydırma ve yakınlaştırma, kare döngüsü, çizicilerin sırayla çağrılması, yapılandırma ve özellik anahtarları, veri penceresi; ev sahibi arayüzü (`NativeHost`) ve test ev sahibi (`TestHost`) |
| `workspace` | `src/workspace` | Çoklu grafik çalışma alanı: yerleşimler, ayırıcılar, grafikler arası eşitleme, kalıcılık, tekrar oynatma, ekran görüntüsü birleştirme, `ChartCell` ve `Workspace`; ev sahibi arayüzü (`WorkspaceHost`) ve test ev sahibi |
| `widget` | `src/widget` | Pencere öğesi kabuğunun mantığı: seçenekler, katkı kayıt defteri, üst çubuk ve kenar çubuğu bileşimi, sembol ve gösterge seçici modelleri, nesne ağacı, bağlam menüsü, durum satırı, panel yuvası, piyasa durumu, seans gölgelendirme, zaman dilimleri |
| `ui`, `plugin` | `src/ui`, `src/plugin.ts` | Klavye haritası, yeniden adlandırılmış kimlikler; eklenti API'si |
| `render::shared` | `src/renderers/shared` | Göstergelerin ürettiği çizgi, kutu, etiket, çoklu çizgi ve dolguların boyayıcısı; tablo boyayıcısı ve yerleşim planı; metin sarma, işlem işaretleri |

## Çevrilmeyenler ve eksikler

- **Betik motoru.** Özgün proje Pine betiklerini ayrı bir motorla çalıştırır; bu motor çeviride yoktur. `addIndicator(kaynak)`, `runScript` ve `registerEngine` çağrıları başarısız sonuç verir ya da açık bir hata fırlatır. Yerleşik 75 gösterge çalışır.
- **Özgün arayüz kiti** (`packages/vela-ui`, yaklaşık 16 bin satır). Legend, pencereler, araç çubukları, seçiciler, paneller ve menüler `vela-web` içinde aynı yapı, sıra, etiket ve davranışla ama yerel HTML öğeleriyle kuruldu: renk seçici bir renk kutusu ile saydamlık çubuğu, açılır listeler `<select>`, tarih ve saat tarayıcının kendi alanları, ipuçlarının bir kısmı `title` özniteliğidir. Kitin çıkış animasyonları, kaydırarak kapatma hareketi ve odak yönetiminin tamamı yoktur.
- **`Vela` sınıfının reddettikleri.** Özel çizici sınıfı, betik motoru, özel biçimleyiciler (`formatters`), fiyat ekseni çentik işlevi, işlev olarak verilen `freePan`, işaretin tembel içerik işlevi. Kurucu bunları yok saymaz, açık bir hata fırlatır.
- **Sayfa düzeyinde olmayan dışa aktarımlar.** `registerChartType`, `registerNativeIndicator`, `registerRendererDefaults`, `registerRendererLayer`, `createTheme`, `checkTheme`, `registerIcon`, `registerDefaultEngine` ve özgün `index.ts`'in dışa açtığı sınıflar (`NativeRenderer`, `DrawingStore`, `MultiProviderFeed` gibi) JavaScript'ten erişilebilir değildir. Bunların Rust karşılıkları çekirdekte vardır.
- **Ev sahibi ayar bölümleri** geri çağrı taşımaz; düzenleme, sarmalayıcı öğede `vela-host-setting` adlı bir DOM olayı olarak bildirilir.
- **Pencere öğesinde eksik kalanlar.** Hücreleri sürükleyerek yer değiştirme tutamacı yalnızca hücre denetimlerindedir; özgün bildirim yığını (`createToaster`) yerine tek bir bildirim şeridi vardır; `VelaWidget` üzerinde `keymap`, `history`, `chart`, `context()` okuyucuları yoktur; `drawings: false` seçeneği ayrıştırılmaz.
- **Çekirdeğin yerleşik `Intl` uygulaması** yalnızca İngilizce, UTC ve sabit ofsetli saat dilimlerini bilir. Tarayıcıda `BrowserIntl` tam veriyi kullanır; tarayıcı dışındaki bir ev sahibi kendi uygulamasını vermelidir.
- **Ekran görüntüsü.** DOM katmanları (legend, durum satırı, filigran) resme işlenir, ama bu işleme yalnızca sahte nesneler üzerinde sınandı.

## Özgün koddan bilinçli farklar

- Ev sahibi bağlamı her yaşam döngüsü çağrısına parametre olarak gelir. Özgün kodda gösterge örneği bağlamı `start` sırasında alıp saklıyordu.
- Gösterge kataloğunu `NativeIndicatorRegistry` değeri olarak siz oluşturur ve taşırsınız. Özgün kodda katalog süreç genelinde tek bir haritaydı.
- `NativeIndicatorContext` içinde piyasa verisi erişimi (`data`) ve özel çizim yükü (`pushData`) yoktur. Bu ikisi henüz çevrilmeyen katmanlara aittir.
- Zaman damgaları `i64` milisaniyedir. Hacmi olmayan çubukta `volume` alanı `None` olur.
- Çizim sınıf hiyerarşisi yerine her araç ortak `DrawingBase` durumunu gömer ve `Drawing` trait'ini uygular. Yalnızca veriyle ayrışan alt sınıflar tek yapıda toplandı (harmonik formasyonlar, pitchfork türleri, Mach figürleri).
- `DrawingStore` değişiklik dinleyicisi çağırmaz. Her değişiklik `revision()` sayacını artırır.
- Ayar yaması tipli alanlara uygulanır. Özgün kod değeri olduğu gibi atar; burada yanlış tipteki değer yok sayılır.
- Kayıtlı özellikler (`props`) okunurken her alanın tipi denetlenir. Özgün kodun dört aracı (regresyon kanalı, sabitlenmiş VWAP, hacim profili, büyüteç) kayıtlı değeri denetlemeden alır.
- `showOn` ayarı bant sırasına dizilerek saklanır. Özgün kod verildiği sırayla saklar, yeniden yüklerken bant sırasına dizer.
- Özgün kod tarayıcının `Intl` API'sini doğrudan çağırır. Çekirdek bunun yerine `intl::Intl` trait'ini çağırır; `vela-web::BrowserIntl` onu tarayıcıya bağlar. Ev sahibi bir uygulama vermezse yerleşik `EnglishIntl` kullanılır: yalnızca İngilizce, UTC ve sabit ofsetli saat dilimleri.
- Özgün kod `Promise`, `setTimeout`, `fetch` ve `WebSocket`'i doğrudan kullanır. Çekirdek bunları `runtime::Platform` trait'inden ister ve tek iş parçacıklı çalışır (`Rc`, `RefCell`, `Send` olmayan future'lar). `vela-web::BrowserPlatform` onu tarayıcıya bağlar.
- JavaScript'te `await` her zaman bir tur bekler; Rust'ta hazır bir future hemen devam eder. Motor ve beslemeler özgün kodun `await` ettiği her yerde `runtime::js_await` kullanır, böylece olayların sırası özgün kodla aynı kalır. Görevler arası sıralama JavaScript'in mikro görev kuyruğuyla birebir aynı değildir.
- `Vela` bir çiziciyi seçeneklerden veya `VelaDeps` üzerinden almak zorundadır. Özgün kod varsayılan olarak kendi tuval çizicisini kurar. Burada çizici bir ev sahibi (`NativeHost`) ister ve çekirdek onu kendisi oluşturamaz; çizicisiz kurulum `VelaError::MissingRenderer` döner. Tarayıcıda `vela-web::VelaApp` çiziciyi `BrowserHost` ile kurup `Vela`'ya verir.
- `NativeRenderer` DOM'a dokunmaz. Tuvalleri, kare isteğini, saatleri ve sayfadaki öğeleri (arka plan, atıf işareti, yükleniyor göstergesi, açıklamalar) `NativeHost` üzerinden ister. Fare ve klavye dinleyicileri `vela-web`'dedir ve kabuğun hareket yöntemlerini çağırır (`pointer_move`, `zoom_to`, `fling` gibi).
- Çizim denetleyicisi araç çubuğunu, satır içi metin düzenleyiciyi, seçim penceresini ve büyüteç menüsünü kendisi kurmaz; durumlarını `NativeHost::drawings_ui` ile ev sahibine bildirir, ev sahibi de kullanıcının yaptığını denetleyicinin `toolbar_*`, `text_editor_*` ve `popup_*` yöntemleriyle geri bildirir.
- Sürükleme sırasında denetleyici çizimin kendi kopyasını değiştirir; depo yalnızca fare bırakılınca tek bir düzenleme alır, böylece her hareket geçmişte tek adım olur. Taslak ve yerleşim bildirimleri çekirdeğe özgün koddan bir kare sonra ulaşır.
- `serde_json` `float_roundtrip` özelliğiyle derlenir; kayıtlı belgedeki sayılar JavaScript'teki gibi en yakın değere ayrıştırılır.
- Kabuğun bütün durumu tek bir `RefCell` içindedir. Abonelere gidecek olaylar kuyruğa alınır ve ödünç bırakıldıktan sonra teslim edilir; ev sahibi, kendi yöntemlerinin içinden çiziciyi geri çağıramaz.
- Özgün kodun modül genelindeki kayıt defterleri (grafik tipleri, yerel göstergeler, çizim araçları, temalar, dil paketleri, çizici varsayılanları, çubuk deposu) `VelaEnv` içinde durur. Aynı ortamdan kurulan grafikler bunları paylaşır.
- Çevirmen yerinde değiştirilmez, yenisiyle değiştirilir: `set_locale` sonrasında `chart.i18n()` yeni çevirmeni döner, eskisini saklayan kod eski ayarları görür.
- Yerel göstergeler ev sahibi bağlamını saklamaz; bir yaşam döngüsü çağrısındaki `emit`, `push_data` ve `set_status` çağrıları tamponlanır ve çağrı döner dönmez sırayla uygulanır.
- Sekme görünürlüğü dinleyicisi yoktur; ev sahibi `on_page_visible()` çağırır.
- Özgün arayüzlerdeki isteğe bağlı yöntemler, varsayılanı `None` dönen trait yöntemleridir. Bir sağlayıcı canlı akış veya sembol bilgisi sunuyorsa `supports_stream` ve `supports_symbol_info` yöntemlerinde de bunu bildirmelidir; kayıt defteri bunu yöntemi çağırarak öğrenemez.
- Paylaşılan çubuk deposu süreç genelinde tek değildir; beslemelere `Rc<RefCell<BarStore>>` olarak siz verirsiniz.
- Dil paketi kayıt defteri (`LocaleRegistry`) süreç genelinde değil, sizin taşıdığınız bir değerdir.
- Saat dilimi etiketleri zamanı parametre alır (`at_ms`); özgün kod `new Date()` okur.
- WebAssembly'de standart kitaplığın saati yoktur. `vela_web::install()` çizimlerin oluşturulma zamanı için `Date.now` saatini kurar.
- Çiziciler tarayıcının `CanvasRenderingContext2D` nesnesi yerine `render::canvas::Canvas2d` trait'ine çizer; `vela-web::BrowserCanvas` onu gerçek bağlama bağlar. Tuvalin piksel boyutu, cihaz piksel oranı ve saat (`performance.now()`, `Date.now()`) çiziciye parametre olarak gelir.
- `Canvas2d` yazı tipini ve metin hizasını geri okutmaz. Tablo boyayıcısı girişteki hizayı parametre olarak alır.
- Gösterge çizimi boyayıcısı çözümleyicilerini (`LayerDeps`) saklamaz, her çağrıda alır.
- Büyüteç aracının zaman dilimi etiketinin konumunu özgün kod çizim nesnesine yazar; burada boyayıcı saklar (`DrawingPainter::chip_rect`).
- Görünür aralığı NaN olan sahnede Canvas 2D arka ucu yalnızca tuvali temizler.
- WebGL2 arka ucu tarayıcının bağlamı yerine `render::gl::Gl` trait'ine çizer; `vela-web::BrowserGl` onu gerçek bağlama bağlar. Daire ve yay köşeleri için `cos`, `sin` ve `hypot`, V8'in sonuçlarıyla bit bit aynı olsun diye çekirdekte yeniden yazıldı.
- Çalışma alanı ve hücreler DOM'a dokunmaz; hücre kaplarını, ızgarayı, ayırıcıları ve bildirimleri `WorkspaceHost` üzerinden ister. Pencere öğesi bileşenlerinin durumu çekirdekteki modellerde durur, tarayıcı katmanı yalnızca öğeleri kurar.
- Bir hücre, paylaşılan beslemeden gelen "sembol çözülemedi" bildirimini yalnızca kendi sembolü için gösterir; özgün kod her hücrede gösterir.
- `destroy()` sonrasında `Vela` sınıfının çağrıları sessizce hiçbir şey yapmaz; özgün kod çağrıyı sökülmüş motora ulaştırır.
- Varsayılan argüman Rust'ta olmadığı için özgün varsayılanlar adlandırılmış sabitlerde durur (`ZONE_TRANSPARENCY`, `KAMA_FAST_ALPHA` gibi).

## Kullanım

```rust
use vela_core::indicators::classics::{classic_native_output, classic_specs};
use vela_core::model::Ohlcv;

let bars: Vec<Ohlcv> = (0..100)
    .map(|i| {
        let close = 100.0 + i as f64;
        Ohlcv::new(i * 60_000, close, close + 1.0, close - 1.0, close, Some(1_000.0))
    })
    .collect();

let specs = classic_specs();
let rsi = specs.iter().find(|s| s.indicator_type == "rsi").unwrap();

// Saf hesap: çubuklarla hizalı değer dizileri, ısınma süresinde NaN.
let out = rsi.compute(&bars, &rsi.default_inputs());
println!("{:?}", out.plots[0].values.last());

// Çizime hazır çıktı: kalıcı seri kimlikleri, dolgular, seviye çizgileri.
let scene = classic_native_output(rsi, &bars, &rsi.default_inputs());
println!("{}", scene.series[0].id);
```

## Derleme ve test

Rust 1.85 veya üstü gerekir (2024 sürümü). Tarayıcı paketi için `wasm32-unknown-unknown` hedefi de gerekir (`rustup target add wasm32-unknown-unknown`).

```powershell
cargo test
```

```powershell
cargo clippy --all-targets
```

Tarayıcı paketini üretmek için (`vela_web.js`, `vela_web_bg.wasm` ve JS parçacıkları `pkg` klasörüne yazılır):

```powershell
cargo run -p xtask -- web pkg
```

Örnek sayfaları görmek için depo kökünden bir HTTP sunucusu başlatın. Dört sayfa var: `demo/index.html` (tek grafik), `demo/workspace.html` (çoklu grafik), `demo/widget.html` (tam pencere öğesi; `?lang=tr` Türkçe açar) ve `demo/api.html` (özgün `Vela` API'siyle gömme örneği). Hepsi sentetik veriyle çalışır, ağ çağrısı yapmaz (ayrıntı `demo/README.md` dosyasında):

```powershell
python -m http.server 8000
```

`xtask`, `wasm-bindgen` komut satırı aracının işini kütüphanesi üzerinden yapar. Ayrı bir araç kurmak gerekmez; GNU araç zincirli Windows'ta o aracın kurulumu `dlltool` eksikliği yüzünden başarısız olur.

## Özgün kodla karşılaştırma

Karşılaştırmalar: göstergeler, çizim araçları, küçük saf işlevler, çeviri ve biçimlendirme, çizicilerin tuval çağrıları, WebGL2 çağrıları. Ayrıca tarayıcı katmanının sahte DOM üzerinde çalışan duman testleri var. Sekiz karşılaştırmanın hepsi 15 Kasım 2026'da, yeniden indirilen özgün kaynağa (`fc3e2a0`) karşı baştan çalıştırıldı ve aşağıdaki sayılarla geçti. Karşılaştırmaların hepsi özgün deponun bir kopyasını (örneğin depo kökünde git dışı tutulan `.upstream` klasörü) ve Node 23.6 veya üstünü ister. Betikler özgün kaynakları Node'un kendi TypeScript desteğiyle çalıştırır; `npm install` gerekmez.

### Göstergeler

`dump_catalog` örneği katalogdaki 75 göstergeyi sabit tohumlu çubuklar üzerinde 840 girdi senaryosuyla hesaplar ve sonucu JSON olarak yazar. `tools/parity/compare.mjs` aynı çubukları ve girdileri özgün TypeScript kataloğuna verir, sonra iki çıktıyı karşılaştırır: başlıklar, girdi şemaları, çizgi değerleri (göreli hata sınırı 1e-9), renkler, dolgular, seviyeler, işaretler.

```powershell
cargo run --release --example dump_catalog > catalog.json
```

```powershell
node tools/parity/compare.mjs <Vela kopyasının yolu> catalog.json
```

12 Kasım 2026'da `fc3e2a0` sürümüne karşı Node 26.13 ile alınan sonuç: 75 gösterge, 840 senaryo, 2.351.712 sayı karşılaştırıldı; en büyük göreli hata 9,44e-14. Başlıklar, girdi şemaları, renkler, dolgular, seviyeler, işaretler ve çoklu çizgiler tek bir istisnayla eşleşti.

İstisna bilinçli: `historical-volatility` göstergesinde renk geçişinin sınırları ısınma süresinde NaN olur. Özgün kod bu çubuklarda geçersiz bir renk metni (`#NaNNaNNaN`) üretir; `shared::gradient` başlangıç rengini döndürür. Betik bu durumu 2.079 renk değerinde gördü ve ayrı satırda raporladı.

### Çizim araçları

`dump_drawings` örneği 79 çizim tipini sabit bir doğrusal projektör üzerinde 1.366 senaryoyla sorgular: 0 çapadan en fazla çapa sayısına kadar çapa kümeleri, her tutamacın sürüklenmesi, gövde taşıma, yerleştirme sonrası düzeltme, şemadaki her ayar alanı için tek tek yamalar ve bozuk kayıtlı özellikler. `tools/parity/compare_drawings.mjs` aynı senaryoları özgün TypeScript çizimlerinde çalıştırır ve şunları karşılaştırır: kayıt defteri girdileri (etiket, simge, varsayılan stil), çapa şeması, tutamaç konumları, sınırlar, fiyat ve zaman aralığı, 273 noktalı ızgarada isabet testi, ayar şeması, düzenlenebilir seviyeler ve serileştirilmiş durum.

```powershell
cargo run --release --example dump_drawings > drawings.json
```

```powershell
node tools/parity/compare_drawings.mjs <Vela kopyasının yolu> drawings.json
```

13 Kasım 2026'da `fc3e2a0` sürümüne karşı Node 26.13 ile alınan sonuç: 79 tip, 1.366 senaryo, 394.248 sayı karşılaştırıldı ve hiçbiri farklı çıkmadı. Betik iki bilinçli sapmayı fark saymaz: `showOn` küme olarak karşılaştırılır, bozuk kayıtlı özelliklerde özgün kodun olduğu gibi sakladığı 26 değer ayrı satırda raporlanır.

Bu karşılaştırma `Drawing` sözleşmesinin ortak yüzeyini kapsar. Araçlara özgü yöntemler (kanal geometrisi, Fibonacci seviye çizgileri, pozisyon istatistikleri, regresyon ve VWAP hesapları, hacim profili) burada sorgulanmaz; boyayıcı onları çağırdığı için aşağıdaki tuval çağrısı karşılaştırması dolaylı olarak kapsar.

### Küçük saf işlevler

`dump_misc` örneği ve `tools/parity/compare_misc.mjs` şunları karşılaştırır: zaman dilimi süreleri (32 girdi), Heikin Ashi (300 çubuk, toplu ve adım adım), genişletilmiş sembol ayrıştırma, dört çubuk kümesinde dokuz görünür aralık ön ayarı, sembol ve grup yardımcıları, 16 durum belgesinin temizlenmesi (biri geçerli, diğerleri bozuk).

```powershell
cargo run --release --example dump_misc > misc.json
```

```powershell
node tools/parity/compare_misc.mjs <Vela kopyasının yolu> misc.json
```

13 Kasım 2026 sonucu: 112 denetim, 3.627 sayı, fark yok. Çubuk deposu, işaretler, olay veri yolu, grafik tipi ayar satırları ve veri yok hatası bu karşılaştırmada yoktur; onlar Rust testlerine dayanır.

### Çeviri ve biçimlendirme

Bu karşılaştırma Rust kodunu WebAssembly'ye derler ve Node içinde çalıştırır; iki taraf da aynı Node'un `Intl` verisini kullanır. `tools/parity/compare_format.mjs` istekleri üretir, özgün TypeScript'e ve `vela-web`'in `parity_run` girişine verir.

```powershell
cargo run -p xtask -- web pkg-parity --parity
```

```powershell
node tools/parity/compare_format.mjs <Vela kopyasının yolu> pkg-parity
```

13 Kasım 2026 sonucu: 113.004 istek, 101 çevirmen yapılandırması, 20 dil paketi; fark yok. Kapsam:

- Katalogdaki her mesaj, 20 dilin her birinde, argümanları dört farklı değere bağlanarak (çoğul dallarına ulaşmak için).
- 31 dil etiketinde (geçersiz etiketler dahil) fiyat, yüzde, kısa sayı, süre, eksen etiketi, zaman damgası ve saat biçimleri; 12 ve 24 saatlik düzen, yerel rakam sistemleri, yedi saat dilimi.
- Ay ve gün adları, yıl etiketi, hafta başlangıcı, yön, yedek zinciri, saat dilimi ofsetleri, büyük harfe çevirme.

Sonuç Node 26.13'ün ICU verisine göredir; başka bir tarayıcının `Intl` verisi farklı metin üretebilir, ama iki taraf da aynı veriyi okuduğu için eşleşme bundan etkilenmez.

### Çizicilerin tuval çağrıları

Çiziciler piksel üzerinden karşılaştırılmaz, tuvale yaptıkları çağrılar karşılaştırılır. Rust tarafı `RecordingCanvas` üzerine çizer; özgün TypeScript çizicileri `tools/parity/recording_canvas.mjs` içindeki kayıt bağlamına çizer. İki kaydedici aynı adları ve aynı sabit metin ölçüsünü kullanır (UTF-16 birimi başına 0,6 em). Betikler iki akışı çağrı çağrı karşılaştırır: ad, sıra ve argümanlar (sayılarda göreli hata sınırı 1e-9).

Çizim araçları: `dump_paint` 79 aracı 3.546 senaryoda boyar (0 çapadan en fazla çapaya, seçili ve üzerine gelinmiş durum, yerleştirme önizlemesi, metinli ve metinsiz, şemadaki her ayar için tek tek yamalar).

```powershell
cargo run --release --example dump_paint > paint.json
```

```powershell
node tools/parity/compare_paint.mjs <Vela kopyasının yolu> paint.json
```

13 Kasım 2026 sonucu: 3.546 senaryo, 237.783 tuval çağrısı, 238.525 sayı; fark yok. Senaryolar büyüteç aracına çubuk verisi yüklemez; büyütecin içine mum çizen yol Rust testlerine dayanır.

Gösterge çizimleri ve tablolar: `dump_scene` rastgele üretilmiş 526 çizim kümesini (çizgi, kutu, etiket, çoklu çizgi, dolgu; 21 etiket stilinin her biri beş dikey yerleşimde) ve 180 tabloyu boyar.

```powershell
cargo run --release --example dump_scene > scene.json
```

```powershell
node tools/parity/compare_scene.mjs <Vela kopyasının yolu> scene.json
```

13 Kasım 2026 sonucu: 41.536 tuval çağrısı, 51.235 sayı; çağrılar, ipucu bölgeleri ve otomatik ölçek aralıkları eşleşti.

Seriler, eksenler ve katmanlar: `dump_series` iki tarafa aynı sahneyi ve koordinat sistemini kurar, 961 senaryo üretir ve yedi çiziciyi çalıştırır: Canvas 2D arka ucu, arka plan, eksenler, artı imleci, işaret şeridi, hacim ve hacim profili katmanları. Senaryolar mum, çubuk, Heikin Ashi ve kapanış stillerini üç ayrıntı düzeyinde; boşluklu, tek çubuklu, boş ve NaN içeren veri kümelerini; 0,75 ile 3 arası cihaz piksel oranlarını; bir ile üç panelde her türden gösterge serisini, dolguları ve fiyat çizgilerini; doğrusal, logaritmik, yüzde ve endeksli fiyat ölçeklerini kapsar. 111 senaryo girdileri bilerek bozar (sıfır aralık, ters ölçek, sıfır yükseklikli panel, okunamayan renk). Çıktı dosyası yaklaşık 170 MB'tır.

```powershell
cargo run --release --example dump_series > series.json
```

```powershell
node tools/parity/compare_series.mjs <Vela kopyasının yolu> series.json
```

13 Kasım 2026 sonucu: 3.690.587 tuval çağrısı, 4.771.166 sayı, en büyük göreli hata 2,33e-12; fark yok. Betik üç bilinçli sapmayı 34 senaryoda gördü ve ayrı saydı: görünür aralığı NaN olan sahnede arka uç yalnızca tuvali temizler (özgün kod kırpma bölgelerini açıp fiyat çizgilerini yine çizer), aynı durumda hacim profili katmanı özgün kod gibi hata fırlatmaz, geçersiz bir tarih `Invalid Date` olarak yazılır (özgün kod `undefined NaN` yazar veya hata fırlatır).

Bu karşılaştırma ilk çalıştırmada bir fark buldu: görünüm milattan sonra 100 yılından öncesine uzandığında özgün kod, JavaScript'in `Date.UTC` davranışı yüzünden takvim çentiklerini kaydırır. Çeviri bu davranışı artık aynen üretir.

Kapsam dışı kalanlar: İngilizce dışındaki diller ve IANA saat dilimleri (çekirdeğin yerleşik `Intl` uygulaması bunları bilmez), işaret simgelerinin gerçek rasterleştirilmesi, eksen çizicisinin isabet testi yöntemleri.

### WebGL2 çağrıları

`dump_gl` üretilmiş sahneleri WebGL2 arka ucuyla `RecordingGl` üzerine çizer; `tools/parity/compare_gl.mjs` aynı sahneleri özgün `WebGL2Backend` üzerinde, GL çağrılarını kaydeden bir JavaScript taklidiyle çalıştırır ve iki akışı karşılaştırır (tolerans sıfır).

```powershell
cargo run --release --example dump_gl > gl.json
```

```powershell
node tools/parity/compare_gl.mjs <Vela kopyasının yolu> gl.json
```

15 Kasım 2026 sonucu: 643 kare, 106.473 GL çağrısı, 170.663 sayı; hepsi aynı. 28 kare iki tarafta da çalıştırılamadı: özgün kod kesikli çizgileri kırpmadan önce parçalara böldüğü için panelin çok dışına uzanan bir çizgi milyonlarca parça üretir ve bellek biter. Karşılaştırma ilk çalıştırmada iki fark bulmuştu: NaN aralıkta fiyat çizgilerinin çizilmemesi ve trigonometri sonuçlarının son basamağı. İkisi de düzeltildi.

### Çizici kabuğu ve tarayıcı

Kabuk (`render::native`) özgün kodla yan yana çalıştırılmadı. Doğruluğu, özgün test dosyalarındaki senaryoların `TestHost` üzerinde çalışan 152 Rust karşılığına dayanır; çağırdığı çiziciler yukarıdaki karşılaştırmalardan geçmiştir.

Tarayıcı bağlantısı iki yolla denendi:

```powershell
node tools/parity/smoke_chart.mjs pkg
```

Duman testleri paketi Node içinde sahte bir DOM ile çalıştırır. 15 Kasım 2026'da hepsi geçti:

| Betik | Kapsam | Denetim |
|---|---|---|
| `smoke_chart.mjs` | Tek grafik: öğe yapısı, girdi, çizim araç çubuğu ve çizimler, tema, dil, kapatma | 166 |
| `smoke_legend.mjs` | Legend, panel ve eksen düğmeleri, ipuçları, menüler | 112 |
| `smoke_dialogs.mjs` | Grafik ayar penceresi, gösterge girdi penceresi, tema satırı | 169 |
| `smoke_drawing_settings.mjs` | Çizim ayar çubuğu ve paneli, 76 aracın paneli | 333 |
| `smoke_workspace.mjs` | Çoklu grafik: yerleşim, etkin hücre, eşitleme, ayırıcılar, kalıcılık, WebGL bütçesi | 126 |
| `smoke_shell_top.mjs` | Üst çubuk, sembol, gösterge ve yerleşim seçiciler, alt çubuk, durum satırı, filigran, hücre denetimleri | 139 |
| `smoke_shell_side.mjs` | Kenar çubuğu, paneller, nesne ağacı, veri penceresi, bağlam menüsü, kısayollar, mobil yüzeyler | 209 |
| `smoke_widget.mjs` | Birleşik pencere öğesi: dil paketleri, katkılar, panel düğmeleri, ekran görüntüsü, pencerelerin kökte açılması | 107 |
| `smoke_gl.mjs` | WebGL2 bağlantısı, canvas2d'ye geri düşme, bağlam kaybı, parlama | 70 |
| `smoke_raster.mjs` | Ekran görüntüsünde DOM katmanlarının işlenmesi | 21 |
| `smoke_vela_api.mjs` | `Vela` sınıfı: her üye, altı denetim yüzeyi, 42 olay, JavaScript veri sağlayıcısı, hatalı kullanım | 251 |

Sahte DOM yerleşim hesaplamaz ve boyamaz; bu testler bağlantıyı ve davranışı sınar, görünümü sınamaz.

Legend, ayar pencereleri ve çizim ayar penceresi ilk yazıldığında özgün kaynak kopyasının bir kısmı eksikti. Üçü, ardından pencere öğesi kabuğu, tam kaynağa karşı satır satır denetlendi; bulunan farklar düzeltildi (legend satır sırası, işaret simgelerinin SVG'si, HTML temizleyicisi, ondalıklı sayı alanları, çoklu seçim klavyesi ve benzerleri) ve özgün testlerden DOM gerektirmeyenler çevrildi (`cargo test -p vela-web`, 67 test). Arayüz kitinin yerine konan yerel öğeler, stil değerlerinin tamamı ve kitin klavye ve odak davranışı özgün kodla karşılaştırılmadı.

Çizim denetleyicisi ve durum makinesi özgün kodla yan yana çalıştırılmadı; özgün testlerin Rust karşılıklarına dayanır (102 test).

Örnek sayfalar 14 ve 15 Kasım 2026'da Chromium tabanlı bir tarayıcıda açıldı ve elle denendi:

- **Tek grafik:** mumlar, EMA, hacim, RSI paneli, eksenler, atıf işareti; tekerlekle yakınlaştırma, sürükleyerek kaydırma, artı imleci, canlı tik, pencere boyutu değişimi; trend çizgisi çizme, seçme, taşıma, silme, geri alma, metin aracı, yenilemede geri yükleme; legend değerleri ve göz düğmesi; grafik ve gösterge ayar pencereleri; çizim ayar çubuğu ve paneli.
- **WebGL2:** grafik `webgl2` arka ucuyla çizildi, yakınlaştırma ve kaydırma yeniden çizdi, mumların arkasındaki çizim doku olarak birleşti, parlama geçişi göründü.
- **Çoklu grafik:** dört hücre kendi sembolüyle çizildi, etkin hücre işaretlendi, yerleşim değişti, durum saklandı.
- **Pencere öğesi:** üst çubuk, sembol arama ve değiştirme, gösterge seçici, nesne ağacı paneli, Türkçe arayüz, yenilemede durumun geri gelmesi.
- **`Vela` API örneği:** iki grafik çizildi.

Bu denemeler yalnızca gerçek tarayıcıda görülen bir çökmeyi ortaya çıkardı ve düzeltildi: SVG öğelerinde `className` metin değildir. Aynı türden olası hatalar için tarayıcı katmanı ayrıca okunarak tarandı.

Bunlar gözle yapılmış denetimlerdir. Görüntü özgün Vela'nın çıktısıyla piksel piksel karşılaştırılmadı. Denenmeyenler: dokunmatik hareketler ve mobil düzen, başka tarayıcılar, açık tema, bağlam menüsü, nesne ağacında sürükleyerek sıralama, ayırıcıları sürükleme, WebGL bağlam kaybı, gerçek borsa sağlayıcıları (ayrıntı `demo/README.md` dosyasında).

### Tarayıcı platformu duman testi

`tools/parity/smoke_platform.mjs`, `BrowserPlatform`'u Node içinde, yerel döngü arayüzünde açtığı geçici bir HTTP sunucusuna karşı çalıştırır: çalışan ve iptal edilen zamanlayıcı, başarılı istek, POST, 404, sinyalle iptal, zaman aşımı, bağlantı hatası.

```powershell
node tools/parity/smoke_platform.mjs pkg-parity
```

13 Kasım 2026'da geçti. WebSocket bu testte yoktur.

### Veri katmanı ve motor

Beslemeler, sağlayıcı kayıt defteri, üç borsa sağlayıcısı, motor orkestratörü, denetleyiciler ve `Vela` giriş sınıfı özgün kodla yan yana çalıştırılmadı. Doğrulukları, özgün test dosyalarındaki senaryoların `ManualPlatform` ve sahte çizici, besleme ve betik motoru üzerinde çalışan Rust karşılıklarına dayanır (motor için 284, `Vela` için 103 test). Gerçek borsa uç noktalarına karşı hiçbir şey çalıştırılmadı: Binance, Coinbase ve Hyperliquid sağlayıcıları yalnızca sahte sunucu yanıtlarıyla test edildi. Motor gerçek çiziciyle örnek sayfalarda, bellekteki beslemelerle çalıştırıldı. Çalışma alanı (159 test) ve pencere öğesi modelleri (476 test) de özgün testlerin Rust karşılıklarına dayanır.

## Lisans

Apache-2.0. Özgün `LICENSE` ve `NOTICE` dosyaları depo kökünde durur. `NOTICE` dosyasındaki atıf koşulu bu çeviri için de geçerlidir: Vela ile çizilen grafiği gösteren ürün, Vela adını ve proje bağlantısını görünür biçimde göstermelidir.
