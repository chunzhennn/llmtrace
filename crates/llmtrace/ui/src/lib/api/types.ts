// TypeScript models mirroring the llmtrace API JSON shapes (storage.rs / api.rs).
// Timestamps are RFC3339 strings; UUIDs are lowercase hyphenated strings.

export type Rfc3339 = string;
export type Uuid = string;

export interface Page {
	limit: number;
	offset: number;
	has_more: boolean;
	next_offset: number | null;
}

export interface Paginated<T> {
	items: T[];
	page: Page;
}

export interface TimeWindow {
	since_hours: number;
	started_at_gte: Rfc3339;
	limit?: number;
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

export type LoginMethod = 'local' | 'oauth';

export interface MeUser {
	user_id: string;
	display_name: string;
	login_method: LoginMethod;
}

export interface MeResponse {
	authenticated: boolean;
	user?: MeUser;
}

// ---------------------------------------------------------------------------
// Stats / runtime
// ---------------------------------------------------------------------------

export interface RuntimeMetrics {
	trace_pipeline: {
		enqueued: number;
		persisted: number;
		dropped_full: number;
		dropped_closed: number;
		build_failures: number;
		persist_failures: number;
		queue_capacity: number;
		queue_available: number;
		queue_depth: number;
	};
	retention: {
		runs: number;
		failures: number;
		last_success_at: Rfc3339 | null;
		last_failure_at: Rfc3339 | null;
		last_error: string | null;
		last_deleted: Record<string, number>;
	};
}

export interface Stats {
	total: number;
	last_hour: number;
	errors: number;
	captured_bytes: number;
	avg_duration_ms: number | null;
	avg_ttft_ms: number | null;
	runtime: RuntimeMetrics;
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

export interface RequestSummary {
	id: Uuid;
	started_at: Rfc3339;
	completed_at: Rfc3339 | null;
	method: string;
	original_uri: string;
	upstream_url: string;
	upstream_host: string | null;
	status: number | null;
	error: string | null;
	request_kind: string;
	model: string | null;
	api_key_hash: string | null;
	session_id: Uuid | null;
	ttft_ms: number | null;
	duration_ms: number | null;
	bytes_in: number;
	bytes_out: number;
	request_body_truncated: boolean;
	response_body_truncated: boolean;
	plugin_metadata: Record<string, unknown>;
	tags: string[];
}

export interface RedactedHeader {
	redacted: true;
	scheme?: string | null;
	sha256?: string | null;
	prefix?: string;
	suffix?: string;
	length?: number;
	url?: string;
}

export type HeaderValue = string | RedactedHeader;

export interface RequestDetail extends RequestSummary {
	session_key: string | null;
	request_headers: Record<string, HeaderValue>;
	response_headers: Record<string, HeaderValue>;
	request_body: string;
	response_body: string;
	request_body_bytes: number;
	response_body_bytes: number;
	content_type: string | null;
}

export interface RequestListResponse extends Paginated<RequestSummary> {}

export interface FacetRow<T = string> {
	value: T;
	request_count: number;
}

export interface RequestFacets {
	window: TimeWindow;
	facets: {
		models: FacetRow[];
		upstream_hosts: FacetRow[];
		request_kinds: FacetRow[];
		statuses: FacetRow<number>[];
		status_classes: FacetRow[];
		error_states: FacetRow<boolean>[];
	};
}

export interface RecentErrorsResponse {
	window: TimeWindow;
	items: RequestSummary[];
	page: { limit: number; has_more: boolean };
}

export interface SlowRequestsResponse {
	window: TimeWindow & { min_duration_ms: number | null };
	items: RequestSummary[];
	page: Page;
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export interface SessionSummary {
	id: Uuid;
	session_key: string;
	first_seen: Rfc3339;
	last_seen: Rfc3339;
	user_id: string | null;
	user_name: string | null;
	request_count: number;
	max_duration_ms: number | null;
}

export interface SessionMessage {
	id: number;
	request_id: Uuid;
	role: string;
	content: string;
	created_at: Rfc3339;
}

export interface SessionDetail {
	id: Uuid;
	session_key: string;
	first_seen: Rfc3339;
	last_seen: Rfc3339;
	user_id: string | null;
	user_name: string | null;
	summary: Record<string, unknown>;
	request_stats: {
		request_count: number;
		error_count: number;
		bytes_in: number;
		bytes_out: number;
		captured_bytes: number;
		avg_duration_ms: number | null;
		max_duration_ms: number | null;
		avg_ttft_ms: number | null;
		max_ttft_ms: number | null;
		first_request_at: Rfc3339 | null;
		last_request_at: Rfc3339 | null;
	};
	messages: SessionMessage[];
	messages_page: Page;
}

export interface SessionRequestsResponse extends Paginated<RequestSummary> {
	session_id: Uuid;
}

// ---------------------------------------------------------------------------
// Analytics
// ---------------------------------------------------------------------------

export interface NamedMetricRow {
	name: string;
	request_count: number;
	error_count: number;
	avg_duration_ms: number | null;
	avg_ttft_ms: number | null;
}

export interface UsageSummary {
	window: TimeWindow & { limit: number };
	totals: {
		request_count: number;
		error_count: number;
		bytes_in: number;
		bytes_out: number;
		captured_bytes: number;
		avg_duration_ms: number | null;
		avg_ttft_ms: number | null;
	};
	top_models: NamedMetricRow[];
	top_upstreams: NamedMetricRow[];
	status_classes: { name: string; request_count: number }[];
	request_kinds: NamedMetricRow[];
}

export interface TimeseriesPoint {
	bucket: Rfc3339;
	request_count: number;
	error_count: number;
	captured_bytes: number;
	avg_duration_ms: number | null;
	avg_ttft_ms: number | null;
}

export interface UsageTimeseries {
	window: { since_hours: number; started_at_gte: Rfc3339; bucket: string };
	points: TimeseriesPoint[];
}

export interface LatencyMetric {
	request_count: number;
	duration_count: number;
	avg_duration_ms: number | null;
	max_duration_ms: number | null;
	p50_duration_ms: number | null;
	p90_duration_ms: number | null;
	p95_duration_ms: number | null;
	p99_duration_ms: number | null;
	ttft_count: number;
	avg_ttft_ms: number | null;
	max_ttft_ms: number | null;
	p50_ttft_ms: number | null;
	p90_ttft_ms: number | null;
	p95_ttft_ms: number | null;
	p99_ttft_ms: number | null;
}

export interface LatencySummary {
	window: TimeWindow;
	totals: LatencyMetric;
	top_upstreams: (LatencyMetric & { name: string })[];
	top_models: (LatencyMetric & { name: string })[];
	request_kinds: (LatencyMetric & { name: string })[];
}

export interface ApiKeyUsageItem {
	api_key_hash: string;
	request_count: number;
	error_count: number;
	session_count: number;
	bytes_in: number;
	bytes_out: number;
	captured_bytes: number;
	avg_duration_ms: number | null;
	max_duration_ms: number | null;
	avg_ttft_ms: number | null;
	max_ttft_ms: number | null;
	first_seen_at: Rfc3339 | null;
	last_seen_at: Rfc3339 | null;
}

export interface ApiKeyUsage {
	window: TimeWindow;
	items: ApiKeyUsageItem[];
}

export interface ModelUsageItem {
	model: string;
	request_count: number;
	error_count: number;
	error_rate: number;
	proxy_error_count: number;
	http_5xx_count: number;
	upstream_count: number;
	api_key_count: number;
	session_count: number;
	bytes_in: number;
	bytes_out: number;
	captured_bytes: number;
	avg_duration_ms: number | null;
	max_duration_ms: number | null;
	avg_ttft_ms: number | null;
	max_ttft_ms: number | null;
	first_seen_at: Rfc3339 | null;
	last_seen_at: Rfc3339 | null;
}

export interface ModelUsage {
	window: TimeWindow;
	items: ModelUsageItem[];
}

export interface UserUsageItem {
	user_id: string | null;
	user_name: string | null;
	request_count: number;
	error_count: number;
	error_rate: number;
	proxy_error_count: number;
	http_5xx_count: number;
	upstream_count: number;
	api_key_count: number;
	session_count: number;
	bytes_in: number;
	bytes_out: number;
	captured_bytes: number;
	avg_duration_ms: number | null;
	max_duration_ms: number | null;
	avg_ttft_ms: number | null;
	max_ttft_ms: number | null;
	first_seen_at: Rfc3339 | null;
	last_seen_at: Rfc3339 | null;
}

export interface UserUsage {
	window: TimeWindow;
	items: UserUsageItem[];
}

export interface UpstreamHealthItem {
	upstream_host: string | null;
	request_count: number;
	error_count: number;
	error_rate: number;
	proxy_error_count: number;
	http_2xx_count: number;
	http_3xx_count: number;
	http_4xx_count: number;
	http_5xx_count: number;
	no_status_count: number;
	session_count: number;
	bytes_in: number;
	bytes_out: number;
	captured_bytes: number;
	duration_count: number;
	avg_duration_ms: number | null;
	max_duration_ms: number | null;
	p95_duration_ms: number | null;
	ttft_count: number;
	avg_ttft_ms: number | null;
	max_ttft_ms: number | null;
	p95_ttft_ms: number | null;
	first_seen_at: Rfc3339 | null;
	last_seen_at: Rfc3339 | null;
}

export interface UpstreamHealth {
	window: TimeWindow;
	items: UpstreamHealthItem[];
}

export interface ErrorMetricRow {
	name: string;
	error_count: number;
	proxy_error_count: number;
	http_5xx_count: number;
	avg_duration_ms: number | null;
	max_duration_ms: number | null;
}

export interface ErrorSummary {
	window: TimeWindow;
	totals: {
		error_count: number;
		proxy_error_count: number;
		http_5xx_count: number;
		affected_sessions: number;
		avg_duration_ms: number | null;
		max_duration_ms: number | null;
		first_seen_at: Rfc3339 | null;
		last_seen_at: Rfc3339 | null;
	};
	sources: ErrorMetricRow[];
	top_upstreams: ErrorMetricRow[];
	top_models: ErrorMetricRow[];
	status_classes: ErrorMetricRow[];
	request_kinds: ErrorMetricRow[];
}

// ---------------------------------------------------------------------------
// Structured query
// ---------------------------------------------------------------------------

export type QueryOp =
	| 'eq'
	| 'ne'
	| 'contains'
	| 'gt'
	| 'gte'
	| 'lt'
	| 'lte'
	| 'is_null'
	| 'is_not_null';

export type SortDirection = 'asc' | 'desc';

export interface QueryFilter {
	field: string;
	op: QueryOp;
	value?: unknown;
}

export interface QueryOrder {
	field: string;
	direction?: SortDirection;
}

export interface StructuredQueryRequest {
	dataset: string;
	fields?: string[];
	filters?: QueryFilter[];
	order_by?: QueryOrder[];
	limit?: number;
}

export interface StructuredQueryResult {
	dataset: string;
	fields: string[];
	rows: Record<string, unknown>[];
	limit: number;
}

export interface FieldSpec {
	name: string;
	filter_kind: string | null;
	operators: QueryOp[];
}

export interface DatasetSpec {
	name: string;
	relation?: string;
	default_fields: string[];
	default_order: QueryOrder[];
	fields: FieldSpec[];
}

export interface QuerySchema {
	limits: {
		max_fields: number;
		max_filters: number;
		max_order_by: number;
		max_limit: number;
		max_string_value_bytes: number;
		max_json_value_bytes: number;
	};
	sort_directions: SortDirection[];
	plugin_metadata: {
		dataset: string;
		field_prefix: string;
		filter_kind: string;
		operators: QueryOp[];
		max_segments: number;
		max_segment_bytes: number;
		segment_pattern: string;
	};
	datasets: DatasetSpec[];
}

// ---------------------------------------------------------------------------
// Audit / UI sessions / plugins / admin
// ---------------------------------------------------------------------------

export interface AuditEvent {
	id: number;
	created_at: Rfc3339;
	event_type: string;
	user_id: string | null;
	remote_addr: string | null;
	detail: Record<string, unknown>;
}

export interface AuditEventsResponse extends Paginated<AuditEvent> {}

export interface AuditSummaryRow {
	name: string;
	event_count: number;
	user_count: number;
	remote_addr_count: number;
	first_seen_at: Rfc3339 | null;
	last_seen_at: Rfc3339 | null;
}

export interface AuditSummary {
	window: TimeWindow;
	totals: {
		event_count: number;
		user_count: number;
		remote_addr_count: number;
		first_seen_at: Rfc3339 | null;
		last_seen_at: Rfc3339 | null;
	};
	event_types: AuditSummaryRow[];
	top_users: AuditSummaryRow[];
	top_remote_addrs: AuditSummaryRow[];
}

export interface UiSession {
	session_hash: string;
	user_id: string;
	display_name: string;
	login_method: string;
	created_at: Rfc3339;
	expires_at: Rfc3339;
	expires_in_secs: number;
	expired: boolean;
}

export interface UiSessionsResponse {
	items: UiSession[];
	page: Page & { include_expired: boolean };
}

export interface PluginItem {
	name: string;
	hooks: string[];
	loaded: boolean;
	error: string | null;
}

export interface PluginsResponse {
	configured_count: number;
	loaded_count: number;
	failed_count: number;
	items: PluginItem[];
}

// ---------------------------------------------------------------------------
// System / config / security / retention / storage / redaction
// ---------------------------------------------------------------------------

export interface RuntimeConfig {
	server: {
		listen: string;
		public_url: string;
		deployment: string;
		ui_enabled: boolean;
	};
	proxy: {
		default_upstream: string;
		allow_upstreams: string[];
		allow_upstreams_count: number;
		upstream_header: string;
		timeout_secs: number;
		max_request_body_bytes: number;
		max_response_body_bytes: number;
		max_websocket_message_bytes: number;
		max_websocket_session_bytes: number;
	};
	archive: {
		storage_backend: string;
		filesystem_root: string;
		segment_uncompressed_bytes: number;
		compression_level: number;
	};
	storage: {
		max_connections: number;
		acquire_timeout_secs: number;
		trace_queue_capacity: number;
		trace_worker_count: number;
		retention_days: number | null;
		retention_prune_interval_secs: number;
		retention_prune_batch_size: number;
	};
	auth: Record<string, unknown>;
	observability: { metrics_bearer_token_configured: boolean };
	redaction: {
		sensitive_header_count: number;
		store_header_hash: boolean;
		body_storage: string;
	};
	plugins: { configured_count: number };
}

export interface PostureCheck {
	id: string;
	status: 'pass' | 'warn' | 'fail';
	message: string;
}

export interface SecurityPosture {
	overall: 'ready' | 'attention' | 'fail';
	counts: { pass: number; warn: number; fail: number };
	checks: PostureCheck[];
}

export interface RetentionStatus {
	enabled: boolean;
	retention_days: number | null;
	cutoff: Rfc3339 | null;
	checked_at: Rfc3339;
	prune_interval_secs: number;
	prune_batch_size: number;
	expired: Record<string, number>;
}

export interface StorageRelation {
	name: string;
	present: boolean;
	estimated_rows: number;
	live_rows_estimate: number;
	dead_rows_estimate: number;
	total_bytes: number;
	table_bytes: number;
	index_bytes: number;
	auxiliary_bytes: number;
	last_vacuum_at: Rfc3339 | null;
	last_autovacuum_at: Rfc3339 | null;
	last_analyze_at: Rfc3339 | null;
	last_autoanalyze_at: Rfc3339 | null;
}

export interface StorageSummary {
	checked_at: Rfc3339;
	totals: {
		relation_count: number;
		present_relation_count: number;
		total_bytes: number;
		table_bytes: number;
		index_bytes: number;
		auxiliary_bytes: number;
	};
	relations: StorageRelation[];
}

export interface RedactionPreviewRequest {
	headers?: Record<string, string>;
	uri?: string;
	body?: string;
}

export interface RedactionPreview {
	redaction: {
		store_header_hash: boolean;
		sensitive_header_count: number;
		upstream_header: string;
		limits: Record<string, number>;
	};
	headers: {
		provided: boolean;
		redacted: Record<string, HeaderValue>;
		first_secret_header_hash: string | null;
	};
	uri: {
		provided: boolean;
		input_bytes?: number;
		output_bytes?: number;
		changed?: boolean;
		redacted: string | null;
	};
	body: {
		provided: boolean;
		input_bytes?: number;
		output_bytes?: number;
		changed?: boolean;
		dropped?: boolean;
		redacted: string | null;
	};
}

export const REQUEST_KINDS = [
	'openai_chat_completions',
	'openai_responses',
	'anthropic_messages',
	'websocket',
	'generic_json',
	'generic_http'
] as const;

export const STATUS_CLASSES = [
	'no_status',
	'1xx',
	'2xx',
	'3xx',
	'4xx',
	'5xx',
	'other'
] as const;
