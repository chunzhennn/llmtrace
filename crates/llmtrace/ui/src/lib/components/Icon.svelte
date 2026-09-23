<script lang="ts" module>
	import Info from '@lucide/svelte/icons/info';
	import Activity from '@lucide/svelte/icons/activity';
	import House from '@lucide/svelte/icons/house';
	import List from '@lucide/svelte/icons/list';
	import MessageCircle from '@lucide/svelte/icons/message-circle';
	import ChartNoAxesColumn from '@lucide/svelte/icons/chart-no-axes-column';
	import Search from '@lucide/svelte/icons/search';
	import Shield from '@lucide/svelte/icons/shield';
	import Key from '@lucide/svelte/icons/key';
	import Box from '@lucide/svelte/icons/box';
	import Settings from '@lucide/svelte/icons/settings';
	import EyeOff from '@lucide/svelte/icons/eye-off';
	import Sun from '@lucide/svelte/icons/sun';
	import Moon from '@lucide/svelte/icons/moon';
	import LogOut from '@lucide/svelte/icons/log-out';
	import Menu from '@lucide/svelte/icons/menu';
	import X from '@lucide/svelte/icons/x';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import Copy from '@lucide/svelte/icons/copy';
	import Download from '@lucide/svelte/icons/download';
	import Upload from '@lucide/svelte/icons/upload';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import CircleCheckBig from '@lucide/svelte/icons/circle-check-big';
	import Inbox from '@lucide/svelte/icons/inbox';
	import Clock from '@lucide/svelte/icons/clock';
	import Database from '@lucide/svelte/icons/database';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Funnel from '@lucide/svelte/icons/funnel';

	// Preserve application names; SVGs and accessibility defaults belong to Lucide.
	const icons: Record<string, typeof Info> = {
		'info': Info,
		'activity': Activity,
		'home': House,
		'list': List,
		'messages': MessageCircle,
		'chart': ChartNoAxesColumn,
		'search': Search,
		'shield': Shield,
		'key': Key,
		'box': Box,
		'settings': Settings,
		'eye-off': EyeOff,
		'sun': Sun,
		'moon': Moon,
		'log-out': LogOut,
		'menu': Menu,
		'x': X,
		'chevron-right': ChevronRight,
		'chevron-left': ChevronLeft,
		'chevron-down': ChevronDown,
		'external': ExternalLink,
		'copy': Copy,
		'download': Download,
		'upload': Upload,
		'refresh': RefreshCw,
		'alert': TriangleAlert,
		'check': CircleCheckBig,
		'inbox': Inbox,
		'clock': Clock,
		'database': Database,
		'plus': Plus,
		'trash': Trash2,
		'filter': Funnel
	};
</script>

<script lang="ts">
	interface Props {
		name: string;
		size?: number;
		class?: string;
	}

	let { name, size = 18, class: className = '' }: Props = $props();
	const Glyph = $derived(Object.hasOwn(icons, name) ? icons[name] : undefined);
</script>

{#if Glyph}
	<Glyph {size} class={className} aria-hidden="true" />
{/if}
