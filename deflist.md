# 可复用能力清单

语义名/别名 | 文件::符号 | 状态 | 一句话限制
---|---|---|---
低层钩子专用消息泵线程 / ll-hook-pump | src/hotkey.rs::start_hook_thread | 可用 | WH_KEYBOARD_LL/WH_MOUSE_LL 只在自行 GetMessage 泵消息的线程上被回调；装在 winit/Slint 事件循环线程上永远收不到输入
钩子心跳+轮询兜底 / hook-liveness-fallback | src/hotkey.rs::start_poller | 可用 | 钩子若吞掉按键则该键永不进入 GetAsyncKeyState，两条触发路径必须互斥（心跳静默 3s 才由轮询接管）
全局按键轮询触发 / async-key-poll | src/hotkey.rs::start_poller | 可用 | 睡眠 8ms 实际约 15ms 粒度，快速轻点可能漏检；不适合毫秒级实时输入
改绑后抑制误触发 / rebind-held-guard | src/hotkey.rs::set_hotkey | 可用 | 仅对“按下时触发”的模型有效：设 HELD=1 假装仍按住，等真实松手再复位
桌面毛玻璃底图 / capture-blur-backdrop | src/backdrop.rs::capture+blur | 可用 | BitBlt+CAPTUREBLT 抓不到硬件覆盖层/受保护内容；blur 为 O(w*h) 三次盒式，大区域在 debug 下明显耗时
覆盖层不抢焦点 / non-activating-overlay | src/main.rs::make_overlay_non_activating | 可用 | WS_EX_NOACTIVATE 对该窗口首次 ShowWindow 无效，需在 show 后把前台窗口换回去
Slint 悬停坐标 / hover-via-pointer-event | ui/overlay.slint::hover-area | 可用 | TouchArea 的 moved 只在指针被抓取（按住）时触发，悬停必须用 pointer-event(kind==move)
Slint 入场动画前置帧 / delay-reveal | src/main.rs::open_overlay_view | 可用 | 同一帧内 show+置终值会跳过所有 animate；需 Timer::single_shot(16ms) 后再置 reveal
配置 BOM 容错 / config-bom-tolerate | src/config.rs::load | 可用 | 只处理开头 BOM，其他编码（UTF-16/GBK）仍会解析失败回落默认值
UI 端到端注入测试 / ui-inject-test | _tools/uitest.ps1 | 可用 | keybd_event/mouse_event 注入；钩子会被系统其它软件吞掉注入键，判定失败前先确认 GetLastInputInfo 是否推进
Slint 设置界面控件库 / settings-widgets | ui/widgets.slint::Palette/SButton/SToggle/SSlider/SLineEdit/SChip/SColorField/SRow/SCard | 可用 | 面向 winit 软件渲染器：不可用 transform-*，动画只落在 opacity/x/y/width/height/颜色/阴影上；控件通过 out property hovered 暴露悬停，供外层行做整行高亮
文本输入框鼠标编辑 / text-field-clip-scroll | ui/widgets.slint::SLineEdit | 可用 | 输入框上任何覆盖层（TouchArea）都会吞掉按下事件，光标只能跳到文本首尾且无法拖选；必须让 TextInput 直接接收鼠标，并用 clip 容器 + cursor-position-changed 做横向滚动
滚动容器内容高度测量 / scrollview-measure-pane | ui/settings.slint::pane+body | 可用 | Flickable 的 content-height 默认由子元素 min-height 推导，被拉伸的布局会得到 0/视口值而不滚动；需先用 height=preferred-height 的 Rectangle 包住布局，再把 content-height 指向它
序号/资源槽自动回收 / lowest-free-slot | src/config.rs::Config::free_id / free_vk / sync_vk | 可用 | 分配时扫描占用集合取最小空位，删除即自动归还；旧配置升级用 Config::normalize_vks 保留历史映射，运行期新增走 free_vk
点击空白处清焦点 / focus-clear-on-click | ui/settings.slint::pane FocusScope | 可用 | Slint 无 clear-focus API，在内容层放一个 FocusScope 靠 focus-on-click 抢走输入框焦点；它自己已聚焦时返回 Ignored，因此之后的空白拖动仍能滚动
按键捕获复用 / chord-capture-shared | src/main.rs::chord_text + AppState::capture_item | 可用 | 全局钩子只发一种 Captured 事件，用 capture_item 区分目标是全局热键还是某项；chord_text 必须是 parse_chord 的逆函数
实时毛玻璃背景 / live-backdrop | src/main.rs::backdrop_tick + src/backdrop.rs::capture_shrunk | 可用 | 抓屏前必须先 WDA_EXCLUDEFROMCAPTURE 排除自身窗口，否则抓到轮盘自己形成反馈；半分辨率抓取（div=2）才能到 30fps；独占全屏/受保护内容会返回空帧
覆盖层只接管指针 / pointer-pin-not-keyboard | src/hotkey.rs::ms_proc + main.rs::open_overlay_view | 可用 | 轮盘期间用 SetCursorPos 钉住指针（游戏轮询到静止）并累加推矢量，键盘照常放行；全键盘吞掉会冻结游戏，体验反而更怪
