// Thin wrapper over lucide-react-native so screens use a stable `Icon.*` API.
// Every icon is drawn at a 1.75pt stroke (Tokens.dc.html), pinned here.
// Icons are decorative to screen readers: the control that holds one carries
// the accessibilityLabel.
import {
  Hash,
  Search,
  Settings,
  Plus,
  SendHorizontal,
  AtSign,
  Users,
  UserPlus,
  Bell,
  Inbox,
  Pencil,
  MoreVertical,
  Ellipsis,
  Lock,
  LockKeyhole,
  Shield,
  Mic,
  MicOff,
  Headphones,
  User,
  LogOut,
  Check,
  Mail,
  Key,
  Smartphone,
  Bookmark,
  CheckCheck,
  ChevronLeft,
  ChevronRight,
  ChevronDown,
  Copy,
  Link2,
  Share2,
  Download,
  AlertCircle,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  Diamond,
  Volume2,
  Info,
  MessagesSquare,
  MessageCircle,
  Flag,
  Globe,
  Reply,
  Smile,
  SmilePlus,
  Trash2,
  X,
  SlidersHorizontal,
  Contrast,
  Paperclip,
  type LucideIcon,
} from "lucide-react-native";
import { I18nManager, View } from "react-native";
import { semantic } from "../theme/tokens";

// The stroke every icon uses.
export const ICON_STROKE = 1.75;

type P = { size?: number; color?: string };

const wrap =
  (C: LucideIcon, defaultSize = 20) =>
  ({ size, color }: P) => (
    <C
      size={size ?? defaultSize}
      color={color ?? semantic.text}
      strokeWidth={ICON_STROKE}
    />
  );

// Yoga mirrors layout under RTL but never the glyph inside an icon, so an
// icon that ENCODES direction ("back", "forward") is flipped here — and only
// those. A magnifier or a bell means the same thing in both directions, and a
// mirrored one would just be wrong (#1074, the same rule as desktop's
// `.rtl-mirror`).
const mirrored =
  (C: LucideIcon, defaultSize = 20) =>
  ({ size, color }: P) => (
    <View style={I18nManager.isRTL ? { transform: [{ scaleX: -1 }] } : undefined}>
      <C
        size={size ?? defaultSize}
        color={color ?? semantic.text}
        strokeWidth={ICON_STROKE}
      />
    </View>
  );

// Keys that existed before the redesign keep their old default sizes so
// unmigrated call sites keep their layout; new keys default to 20.
export const Icon = {
  /* ── Navigation ── */
  back: mirrored(ChevronLeft, 14),
  fwd: mirrored(ChevronRight, 14),
  chevronLeft: mirrored(ChevronLeft, 24),
  chevronRight: mirrored(ChevronRight, 18),
  chevronDown: wrap(ChevronDown, 18),
  arrowLeft: mirrored(ArrowLeft, 14),
  arrowRight: mirrored(ArrowRight, 14),
  arrowUp: wrap(ArrowUp),
  close: wrap(X),
  x: wrap(X),

  /* ── Tabs ── */
  users: wrap(Users, 24),
  messageCircle: wrap(MessageCircle, 24),
  search: wrap(Search, 16),
  user: wrap(User, 14),

  /* ── Actions ── */
  plus: wrap(Plus, 14),
  send: wrap(SendHorizontal, 16),
  reply: wrap(Reply),
  smile: wrap(Smile),
  smilePlus: wrap(SmilePlus),
  pencil: wrap(Pencil),
  edit: wrap(Pencil, 14),
  copy: wrap(Copy, 14),
  link: wrap(Link2, 14),
  share: wrap(Share2, 14),
  download: wrap(Download, 14),
  trash: wrap(Trash2),
  logOut: wrap(LogOut),
  exit: wrap(LogOut, 14),
  userPlus: wrap(UserPlus),
  attach: wrap(Paperclip),
  more: wrap(Ellipsis),
  kebab: wrap(MoreVertical, 14),

  /* ── Objects ── */
  hash: wrap(Hash, 14),
  at: wrap(AtSign, 14),
  people: wrap(Users, 14),
  bell: wrap(Bell, 14),
  globe: wrap(Globe),
  bookmark: wrap(Bookmark, 14),
  shield: wrap(Shield, 14),
  lock: wrap(Lock, 12),
  lockKeyhole: wrap(LockKeyhole),
  mail: wrap(Mail, 14),
  sliders: wrap(SlidersHorizontal),
  appearance: wrap(Contrast),
  gear: wrap(Settings, 16),
  inbox: wrap(Inbox, 14),
  speak: wrap(Volume2, 14),
  mic: wrap(Mic, 14),
  micOff: wrap(MicOff, 14),
  headphones: wrap(Headphones, 14),
  key: wrap(Key, 14),
  device: wrap(Smartphone, 14),
  thread: wrap(MessagesSquare, 14),
  flag: wrap(Flag, 14),
  info: wrap(Info, 16),
  alert: wrap(AlertCircle, 14),
  diamond: wrap(Diamond, 16),

  /* ── State ── */
  check: wrap(Check, 12),
  checkCheck: wrap(CheckCheck, 12),
};

export type IconName = keyof typeof Icon;
