// Stable `Icon.*` wrapper over lucide-react-native at a 1.75pt stroke. Decorative to screen readers.
// One deep import per icon (#2187), never the "lucide-react-native" barrel: Metro does not
// tree-shake, so the barrel bundles all ~1,800 icons and builds each at first import. The file
// is the icon's canonical name (`MoreVertical` lives in ellipsis-vertical);
// `node scripts/lucide-deep-imports.mjs` rewrites a barrel import, and
// tests/lucide-deep-imports.test.ts fails on a barrel import or a wrong file anywhere in the app.
import Bot from "lucide-react-native/icons/bot";
import Server from "lucide-react-native/icons/server";
import Hash from "lucide-react-native/icons/hash";
import Search from "lucide-react-native/icons/search";
import Settings from "lucide-react-native/icons/settings";
import Plus from "lucide-react-native/icons/plus";
import SendHorizontal from "lucide-react-native/icons/send-horizontal";
import AtSign from "lucide-react-native/icons/at-sign";
import Users from "lucide-react-native/icons/users";
import UserPlus from "lucide-react-native/icons/user-plus";
import Bell from "lucide-react-native/icons/bell";
import BellOff from "lucide-react-native/icons/bell-off";
import Inbox from "lucide-react-native/icons/inbox";
import Pencil from "lucide-react-native/icons/pencil";
import MoreVertical from "lucide-react-native/icons/ellipsis-vertical";
import Ellipsis from "lucide-react-native/icons/ellipsis";
import Lock from "lucide-react-native/icons/lock";
import LockKeyhole from "lucide-react-native/icons/lock-keyhole";
import Shield from "lucide-react-native/icons/shield";
import ShieldCheck from "lucide-react-native/icons/shield-check";
import ShieldAlert from "lucide-react-native/icons/shield-alert";
import ShieldEllipsis from "lucide-react-native/icons/shield-ellipsis";
import ShieldQuestion from "lucide-react-native/icons/shield-question-mark";
import Mic from "lucide-react-native/icons/mic";
import MicOff from "lucide-react-native/icons/mic-off";
import Headphones from "lucide-react-native/icons/headphones";
import User from "lucide-react-native/icons/user";
import LogOut from "lucide-react-native/icons/log-out";
import Check from "lucide-react-native/icons/check";
import Mail from "lucide-react-native/icons/mail";
import Key from "lucide-react-native/icons/key";
import ScanFace from "lucide-react-native/icons/scan-face";
import FingerprintPattern from "lucide-react-native/icons/fingerprint-pattern";
import Smartphone from "lucide-react-native/icons/smartphone";
import Bookmark from "lucide-react-native/icons/bookmark";
import Pin from "lucide-react-native/icons/pin";
import PinOff from "lucide-react-native/icons/pin-off";
import CheckCheck from "lucide-react-native/icons/check-check";
import ChevronLeft from "lucide-react-native/icons/chevron-left";
import ChevronRight from "lucide-react-native/icons/chevron-right";
import ChevronDown from "lucide-react-native/icons/chevron-down";
import Copy from "lucide-react-native/icons/copy";
import Link2 from "lucide-react-native/icons/link-2";
import Share2 from "lucide-react-native/icons/share-2";
import Download from "lucide-react-native/icons/download";
import ExternalLink from "lucide-react-native/icons/external-link";
import AlertCircle from "lucide-react-native/icons/circle-alert";
import ArrowLeft from "lucide-react-native/icons/arrow-left";
import ArrowRight from "lucide-react-native/icons/arrow-right";
import ArrowUp from "lucide-react-native/icons/arrow-up";
import Volume2 from "lucide-react-native/icons/volume-2";
import Info from "lucide-react-native/icons/info";
import MessagesSquare from "lucide-react-native/icons/messages-square";
import MessageCircle from "lucide-react-native/icons/message-circle";
import Flag from "lucide-react-native/icons/flag";
import Globe from "lucide-react-native/icons/globe";
import Reply from "lucide-react-native/icons/reply";
import Smile from "lucide-react-native/icons/face-slightly-smiling";
import SmilePlus from "lucide-react-native/icons/face-slightly-smiling-plus";
import Trash2 from "lucide-react-native/icons/trash";
import X from "lucide-react-native/icons/x";
import SlidersHorizontal from "lucide-react-native/icons/sliders-horizontal";
import Contrast from "lucide-react-native/icons/contrast";
import Paperclip from "lucide-react-native/icons/paperclip";
import Delete from "lucide-react-native/icons/delete";
import UserX from "lucide-react-native/icons/user-x";
import Ban from "lucide-react-native/icons/ban";
import Play from "lucide-react-native/icons/play";
import Pause from "lucide-react-native/icons/pause";
import Film from "lucide-react-native/icons/film";
import Eye from "lucide-react-native/icons/eye";
import EyeOff from "lucide-react-native/icons/eye-off";
import File from "lucide-react-native/icons/file";
import ImageDown from "lucide-react-native/icons/image-down";
import RotateCcw from "lucide-react-native/icons/rotate-ccw";
import RotateCw from "lucide-react-native/icons/rotate-cw";
import Stethoscope from "lucide-react-native/icons/stethoscope";
import Scale from "lucide-react-native/icons/scale";
import Vault from "lucide-react-native/icons/vault";
import Images from "lucide-react-native/icons/images";
import Camera from "lucide-react-native/icons/camera";
import MessageSquare from "lucide-react-native/icons/message-square";
import History from "lucide-react-native/icons/rotate-ccw-clock";
import Laptop from "lucide-react-native/icons/laptop";
import Clock from "lucide-react-native/icons/clock";
import Sprout from "lucide-react-native/icons/sprout";
import Pizza from "lucide-react-native/icons/pizza";
import Gamepad2 from "lucide-react-native/icons/gamepad-2";
import Plane from "lucide-react-native/icons/plane";
import Lightbulb from "lucide-react-native/icons/lightbulb";
import Folder from "lucide-react-native/icons/folder";
import FolderPlus from "lucide-react-native/icons/folder-plus";
import Menu from "lucide-react-native/icons/menu";
import type { LucideIcon } from "lucide-react-native";
import { I18nManager, View } from "react-native";
import { semantic } from "../theme/tokens";

const ICON_STROKE = 1.75;

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

// Only icons that ENCODE direction are mirrored under RTL (#1074).
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

// Pre-redesign keys keep their old default sizes; new keys default to 20.
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
  reply: mirrored(Reply),
  smile: wrap(Smile),
  smilePlus: wrap(SmilePlus),
  pencil: wrap(Pencil),
  edit: wrap(Pencil, 14),
  copy: wrap(Copy, 14),
  link: wrap(Link2, 14),
  share: wrap(Share2, 14),
  download: wrap(Download, 14),
  external: wrap(ExternalLink, 16),
  trash: wrap(Trash2),
  logOut: wrap(LogOut),
  exit: wrap(LogOut, 14),
  userPlus: wrap(UserPlus),
  attach: wrap(Paperclip),
  more: wrap(Ellipsis),
  kebab: wrap(MoreVertical, 14),
  // Points at the text it erases, so it mirrors under RTL.
  backspace: mirrored(Delete, 26),
  userX: wrap(UserX),
  ban: wrap(Ban),

  /* ── Objects ── */
  hash: wrap(Hash, 14),
  // Channel categories (#2031): a category, a new one, and Edit mode's drag handle (≡).
  folder: wrap(Folder, 14),
  folderPlus: wrap(FolderPlus),
  dragHandle: wrap(Menu),
  at: wrap(AtSign, 14),
  people: wrap(Users, 14),
  bell: wrap(Bell, 14),
  bellOff: wrap(BellOff, 14),
  globe: wrap(Globe),
  bookmark: wrap(Bookmark, 14),
  pin: wrap(Pin),
  pinOff: wrap(PinOff),
  shield: wrap(Shield, 14),
  shieldCheck: wrap(ShieldCheck, 14),
  shieldAlert: wrap(ShieldAlert, 14),
  shieldEllipsis: wrap(ShieldEllipsis, 14),
  shieldQuestion: wrap(ShieldQuestion, 14),
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
  // Biometric unlock (#1685): Face ID on iOS, fingerprint on Android.
  faceId: wrap(ScanFace),
  fingerprint: wrap(FingerprintPattern),
  device: wrap(Smartphone, 14),
  laptop: wrap(Laptop, 14),
  // Bots (#1182): the bot itself, and one of the machines that run it.
  bot: wrap(Bot),
  server: wrap(Server),
  history: wrap(History, 14),
  thread: wrap(MessagesSquare, 14),
  flag: wrap(Flag, 14),
  info: wrap(Info, 16),
  alert: wrap(AlertCircle, 14),
  diagnostics: wrap(Stethoscope, 14),
  openSource: wrap(Scale, 14),
  vault: wrap(Vault),
  images: wrap(Images),
  camera: wrap(Camera),
  messageSquare: wrap(MessageSquare),

  /* ── Media (#1248) ── */
  play: wrap(Play),
  pause: wrap(Pause),
  video: wrap(Film),
  eye: wrap(Eye, 16),
  eyeOff: wrap(EyeOff, 16),
  file: wrap(File),
  saveImage: wrap(ImageDown),
  // The arrows encode time, not reading direction: not mirrored.
  seekBack: wrap(RotateCcw),
  seekForward: wrap(RotateCw),

  /* ── Emoji picker categories (desktop's EmojiCategoryRail set, #1972) ── */
  clock: wrap(Clock),
  sprout: wrap(Sprout),
  pizza: wrap(Pizza),
  gamepad: wrap(Gamepad2),
  plane: wrap(Plane),
  lightbulb: wrap(Lightbulb),

  /* ── State ── */
  check: wrap(Check, 12),
  checkCheck: wrap(CheckCheck, 12),
};

// So a control can tell an icon element it may recolour from a composed node.
const ICON_COMPONENTS: ReadonlySet<unknown> = new Set<unknown>(Object.values(Icon));

export function isIconElement(node: unknown): boolean {
  return (
    typeof node === "object" &&
    node !== null &&
    "type" in node &&
    ICON_COMPONENTS.has((node as { type: unknown }).type)
  );
}
