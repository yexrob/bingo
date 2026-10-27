using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Windows.Automation;

namespace Bingo.ImagePaste {
    public static class Protocol {
        public const string Ready = "BINGO_IMAGE_PASTE_READY_V1";
        public static bool MarkerValid(uint owner, string marker) {
            return owner != 0 && marker != null && marker.Length <= 100
                && System.Text.RegularExpressions.Regex.IsMatch(marker,
                    "\\A\\[bingo-image-paste-" + owner + "-[0-9a-f]+-[0-9a-f]+\\]\\z");
        }
    }

    public static class ClipboardPolicy {
        public static bool ImageOnly(Func<uint, bool> available) {
            // Rich selections can offer both text and bitmap. Preserve the
            // terminal's ordinary text paste rather than choosing an image.
            if (available(1) || available(7) || available(13)) return false;
            return available(2) || available(8) || available(17);
        }
    }

    public interface IPasteTarget {
        bool Chord();
        bool Scope();
        bool Focused();
        bool Bitmap();
        bool Inject();
    }

    public sealed class PastePolicy {
        readonly IPasteTarget target;
        bool held;
        public PastePolicy(IPasteTarget target) { this.target = target; }

        public bool Key(int message, uint key, uint flags) {
            if (key != 0x56 || (flags & 0x12) != 0) return false;
            bool down = message == 0x100;
            bool up = message == 0x101;
            if (!down && !up) return false;
            try {
                if (!target.Chord() || !target.Scope() || !target.Focused()) {
                    held = false;
                    return false;
                }
                if (up) {
                    bool previous = held;
                    held = false;
                    return previous;
                }
                if (held) return true;
                if (!target.Bitmap() || !target.Focused() || !target.Scope()) return false;
                held = target.Inject();
                return held;
            } catch {
                held = false;
                return false;
            }
        }
    }

    // All UIA calls stay on this one background MTA. A stuck provider cannot grow
    // threads, supply a cached positive result, or block a key beyond the budget.
    public sealed class FocusProbe : IDisposable {
        readonly AutoResetEvent wake = new AutoResetEvent(false);
        readonly object gate = new object();
        readonly string marker;
        readonly Thread worker;
        readonly Func<bool> query;
        readonly ManualResetEvent ready = new ManualResetEvent(false);
        Request pending;
        bool busy;
        bool stopped;
        sealed class Request {
            public readonly ManualResetEvent Done = new ManualResetEvent(false);
            public bool Match;
            public bool Expired;
        }

        public FocusProbe(string marker) : this(marker, null) { }
        public FocusProbe(string marker, Func<bool> query) {
            this.marker = marker;
            this.query = query ?? Query;
            worker = new Thread(Work);
            worker.IsBackground = true;
            worker.SetApartmentState(ApartmentState.MTA);
            worker.Start();
            if (!ready.WaitOne(500)) throw new InvalidOperationException();
        }

        void Work() {
            ready.Set();
            while (true) {
                wake.WaitOne();
                Request request;
                lock (gate) {
                    if (stopped) return;
                    request = pending;
                }
                bool match = false;
                try { match = query(); } catch { }
                lock (gate) {
                    if (!request.Expired) {
                        request.Match = match;
                        request.Done.Set();
                    } else request.Done.Dispose();
                    busy = false;
                    pending = null;
                }
            }
        }

        bool Query() {
            AutomationElement element = AutomationElement.FocusedElement;
            if (element == null) return false;
            AutomationElement.AutomationElementInformation current = element.Current;
            // WT exposes its pane title in HelpText. Never read Name, text/value
            // patterns, descendants, or terminal document contents.
            return PaneMatches(current.HasKeyboardFocus, current.ClassName, current.HelpText, marker);
        }

        public static bool PaneMatches(bool focused, string className, string helpText, string marker) {
            return focused && className == "TermControl" && !String.IsNullOrEmpty(marker)
                && helpText != null && helpText.EndsWith(marker, StringComparison.Ordinal);
        }

        public bool Matches() {
            Request request;
            lock (gate) {
                if (busy || stopped) return false;
                request = new Request();
                pending = request;
                busy = true;
                wake.Set();
            }
            bool done = request.Done.WaitOne(60);
            lock (gate) {
                if (!done) {
                    request.Expired = true;
                    if (!busy) request.Done.Dispose();
                    return false;
                }
                bool match = request.Match;
                request.Done.Dispose();
                return match;
            }
        }

        public void Dispose() {
            lock (gate) { stopped = true; wake.Set(); }
            // Do not join a potentially stuck external provider or dispose its wait.
        }
    }

    public static class Native {
        public delegate IntPtr HookProc(int code, IntPtr message, IntPtr data);
        [StructLayout(LayoutKind.Sequential)]
        public struct HookKey { public uint Key, Scan, Flags, Time; public UIntPtr Extra; }
        [StructLayout(LayoutKind.Explicit, CharSet = CharSet.Unicode, Size = 20)]
        public struct InputRecord {
            [FieldOffset(0)] public ushort Type;
            [FieldOffset(4)] public int Down;
            [FieldOffset(8)] public ushort Repeat;
            [FieldOffset(10)] public ushort Key;
            [FieldOffset(12)] public ushort Scan;
            [FieldOffset(14)] public char Character;
            [FieldOffset(16)] public uint Modifiers;
        }
        [StructLayout(LayoutKind.Sequential)]
        public struct Message {
            public IntPtr Window;
            public uint Id;
            public UIntPtr WParam;
            public IntPtr LParam;
            public uint Time;
            public int X, Y;
            public uint Private;
        }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        public struct ProcessEntry {
            public uint Size, Usage, ProcessId;
            public UIntPtr Heap;
            public uint ModuleId, Threads, ParentId;
            public int BasePriority;
            public uint Flags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string Exe;
        }
        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
        [DllImport("kernel32.dll")] public static extern uint WaitForSingleObject(IntPtr handle, uint timeout);
        [DllImport("kernel32.dll")]
        public static extern bool GetProcessTimes(IntPtr handle, out long created, out long exited, out long kernel, out long user);
        [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll")] public static extern bool FreeConsole();
        [DllImport("kernel32.dll")] public static extern bool AttachConsole(uint pid);
        [DllImport("kernel32.dll")] public static extern bool SetConsoleCtrlHandler(IntPtr handler, bool add);
        [DllImport("kernel32.dll")] public static extern uint GetConsoleProcessList([Out] uint[] ids, uint count);
        [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow();
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
        public static extern uint GetConsoleTitleW(StringBuilder title, uint size);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll")] public static extern bool GetConsoleMode(IntPtr input, out uint mode);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
        public static extern bool WriteConsoleInputW(IntPtr input, InputRecord[] records, uint count, out uint written);
        [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr GetModuleHandleW(string name);
        [DllImport("kernel32.dll")] public static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
        public static extern bool Process32FirstW(IntPtr snapshot, ref ProcessEntry entry);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
        public static extern bool Process32NextW(IntPtr snapshot, ref ProcessEntry entry);
        [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr window, uint command);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern int GetWindowTextW(IntPtr window, StringBuilder title, int size);
        [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int key);
        [DllImport("user32.dll")] public static extern bool IsClipboardFormatAvailable(uint format);
        [DllImport("user32.dll")] public static extern uint MapVirtualKeyW(uint key, uint kind);
        [DllImport("user32.dll", SetLastError = true)]
        public static extern IntPtr SetWindowsHookExW(int id, HookProc callback, IntPtr module, uint thread);
        [DllImport("user32.dll")] public static extern bool UnhookWindowsHookEx(IntPtr hook);
        [DllImport("user32.dll")] public static extern IntPtr CallNextHookEx(IntPtr hook, int code, IntPtr message, IntPtr data);
        [DllImport("user32.dll")] public static extern int GetMessageW(out Message message, IntPtr window, uint min, uint max);
        [DllImport("user32.dll")] public static extern UIntPtr SetTimer(IntPtr window, UIntPtr id, uint interval, IntPtr callback);
        [DllImport("user32.dll")] public static extern bool KillTimer(IntPtr window, UIntPtr id);
        [DllImport("user32.dll")] public static extern bool PostThreadMessageW(uint thread, uint message, UIntPtr wparam, IntPtr lparam);

        public static bool ValidHandle(IntPtr handle) { return handle != IntPtr.Zero && handle != new IntPtr(-1); }
        public static bool Alive(IntPtr handle) { return ValidHandle(handle) && WaitForSingleObject(handle, 0) == 258; }
        public static IntPtr OpenInput() { return CreateFileW("CONIN$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero); }
        public static bool Raw(IntPtr input) {
            uint mode;
            return GetConsoleMode(input, out mode) && (mode & 7) == 0;
        }
        public static bool Contains(uint pid) {
            uint[] ids = new uint[256];
            uint count = GetConsoleProcessList(ids, (uint)ids.Length);
            if (count == 0 || count > ids.Length) return false;
            for (int i = 0; i < count; i++) if (ids[i] == pid) return true;
            return false;
        }
        public static uint Parent(uint pid) {
            IntPtr snapshot = CreateToolhelp32Snapshot(2, 0);
            if (!ValidHandle(snapshot)) return 0;
            try {
                ProcessEntry entry = new ProcessEntry();
                entry.Size = (uint)Marshal.SizeOf(typeof(ProcessEntry));
                if (!Process32FirstW(snapshot, ref entry)) return 0;
                do { if (entry.ProcessId == pid) return entry.ParentId; }
                while (Process32NextW(snapshot, ref entry));
                return 0;
            } finally { CloseHandle(snapshot); }
        }
        public static bool Inject(IntPtr input) {
            InputRecord[] records = new InputRecord[2];
            for (int i = 0; i < records.Length; i++) {
                records[i].Type = 1;
                records[i].Down = i == 0 ? 1 : 0;
                records[i].Repeat = 1;
                records[i].Key = 0x56;
                records[i].Scan = (ushort)MapVirtualKeyW(0x56, 0);
                records[i].Character = '\x16';
                records[i].Modifiers = 8;
            }
            uint written;
            return WriteConsoleInputW(input, records, 2, out written) && written == 2;
        }
    }

    public sealed class Target : IPasteTarget, IDisposable {
        readonly uint ownerId;
        readonly string marker;
        IntPtr owner, parent, input;
        Mutex consoleLock;
        FocusProbe focus;
        public Target(uint ownerId, string marker) {
            this.ownerId = ownerId;
            this.marker = marker;
            try { Initialize(); } catch { Dispose(); throw; }
        }
        void Initialize() {
            owner = Native.OpenProcess(0x101000, false, ownerId);
            uint parentId = Native.Parent(ownerId);
            if (parentId == 0) throw new InvalidOperationException();
            parent = Native.OpenProcess(0x101000, false, parentId);
            long ownerCreated, parentCreated, ignored1, ignored2, ignored3;
            if (!Native.Alive(owner) || !Native.Alive(parent)
                || !Native.GetProcessTimes(owner, out ownerCreated, out ignored1, out ignored2, out ignored3)
                || !Native.GetProcessTimes(parent, out parentCreated, out ignored1, out ignored2, out ignored3)
                || parentCreated > ownerCreated) throw new InvalidOperationException();
            Native.FreeConsole();
            if (!Native.AttachConsole(ownerId)) throw new InvalidOperationException();
            Native.SetConsoleCtrlHandler(IntPtr.Zero, true);
            input = Native.OpenInput();
            if (!Healthy()) throw new InvalidOperationException();
            bool created;
            consoleLock = new Mutex(false, "Local\\BingoImagePasteConsole-" + Native.GetConsoleWindow().ToInt64(), out created);
            if (!created) throw new InvalidOperationException();
            focus = new FocusProbe(marker);
        }
        public bool Healthy() {
            return Native.Alive(owner) && Native.Alive(parent) && Native.Contains(ownerId) && Native.Raw(input);
        }
        static bool Down(int key) { return (Native.GetAsyncKeyState(key) & 0x8000) != 0; }
        public static bool ChordMatches(bool control, bool shift, bool alt, bool leftWin, bool rightWin) {
            return control && !shift && !alt && !leftWin && !rightWin;
        }
        public bool Chord() {
            return ChordMatches(Down(0x11), Down(0x10), Down(0x12), Down(0x5B), Down(0x5C));
        }
        public bool Scope() {
            if (!Healthy()) return false;
            IntPtr console = Native.GetConsoleWindow();
            IntPtr foreground = Native.GetForegroundWindow();
            if (console == IntPtr.Zero || foreground == IntPtr.Zero) return false;
            if (foreground != console && foreground != Native.GetWindow(console, 4)) return false;
            StringBuilder title = new StringBuilder(32768);
            StringBuilder visible = new StringBuilder(32768);
            if (Native.GetConsoleTitleW(title, (uint)title.Capacity) == 0) return false;
            if (Native.GetWindowTextW(foreground, visible, visible.Capacity) == 0) return false;
            return title.ToString().EndsWith(marker, StringComparison.Ordinal) && title.ToString() == visible.ToString();
        }
        public bool Focused() {
            IntPtr before = Native.GetForegroundWindow();
            if (before == IntPtr.Zero) return false;
            bool match = before == Native.GetConsoleWindow() || focus.Matches();
            return match && before == Native.GetForegroundWindow() && Scope();
        }
        public bool Bitmap() { return ClipboardPolicy.ImageOnly(Native.IsClipboardFormatAvailable); }
        public bool Inject() { return Chord() && Scope() && Bitmap() && Native.Inject(input); }
        public void Dispose() {
            if (focus != null) { focus.Dispose(); focus = null; }
            if (consoleLock != null) { consoleLock.Dispose(); consoleLock = null; }
            if (Native.ValidHandle(input)) Native.CloseHandle(input);
            if (Native.ValidHandle(owner)) Native.CloseHandle(owner);
            if (Native.ValidHandle(parent)) Native.CloseHandle(parent);
            input = owner = parent = IntPtr.Zero;
        }
    }

    public sealed class Companion : IDisposable {
        readonly Target target;
        readonly PastePolicy policy;
        readonly Native.HookProc callback;
        IntPtr hook;
        UIntPtr timer;
        readonly uint thread;
        public Companion(uint owner, string marker) {
            target = new Target(owner, marker);
            policy = new PastePolicy(target);
            callback = Hook;
            thread = Native.GetCurrentThreadId();
        }
        IntPtr Hook(int code, IntPtr message, IntPtr data) {
            if (code >= 0) {
                try {
                    Native.HookKey key = (Native.HookKey)Marshal.PtrToStructure(data, typeof(Native.HookKey));
                    if (policy.Key(message.ToInt32(), key.Key, key.Flags)) return new IntPtr(1);
                } catch { }
            }
            return Native.CallNextHookEx(hook, code, message, data);
        }
        public void Run(Stream input, TextWriter output) {
            hook = Native.SetWindowsHookExW(13, callback, Native.GetModuleHandleW(null), 0);
            if (hook == IntPtr.Zero) throw new InvalidOperationException();
            timer = Native.SetTimer(IntPtr.Zero, UIntPtr.Zero, 250, IntPtr.Zero);
            if (timer == UIntPtr.Zero) throw new InvalidOperationException();
            Thread lifetime = new Thread(delegate() {
                try { while (input.ReadByte() != -1) { } } catch { }
                Native.PostThreadMessageW(thread, 0x12, UIntPtr.Zero, IntPtr.Zero);
            });
            lifetime.IsBackground = true;
            lifetime.Start();
            output.WriteLine(Protocol.Ready);
            output.Flush();
            Native.Message message;
            while (Native.GetMessageW(out message, IntPtr.Zero, 0, 0) > 0) {
                if (message.Id == 0x113 && !target.Healthy()) break;
            }
        }
        public void Dispose() {
            if (hook != IntPtr.Zero) { Native.UnhookWindowsHookEx(hook); hook = IntPtr.Zero; }
            if (timer != UIntPtr.Zero) { Native.KillTimer(IntPtr.Zero, timer); timer = UIntPtr.Zero; }
            target.Dispose();
            GC.KeepAlive(callback);
        }
        public static int Start(uint owner, string marker) {
            // Capture inherited pipes before attaching to the owner's console.
            Stream input = Console.OpenStandardInput();
            StreamWriter output = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false));
            try {
                if (!Protocol.MarkerValid(owner, marker)) return 2;
                using (Companion companion = new Companion(owner, marker)) companion.Run(input, output);
                return 0;
            } catch { return 1; }
        }
    }
}
