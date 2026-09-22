import { expect, it } from 'vitest';
import requestFixture from './fixtures/request-detail.json';
import sessionFixture from './fixtures/session-detail.json';
import messageFixture from './fixtures/session-message.json';
import type { BodyStatus, RequestDetail, SessionDetail, SessionMessage } from './types';

function bodyStatus(value: unknown): BodyStatus | null {
	if (value === null || value === 'available' || value === 'missing' || value === 'unreadable') return value;
	throw new Error(`Invalid body status: ${value}`);
}

it('accepts the same request and session contracts as the Rust serializers', () => {
	// Structural assignment checks required fields and nullability without casting
	// the JSON payload to the expected interface. Status strings are validated above.
	const request: RequestDetail = {
		...requestFixture,
		request_body_status: bodyStatus(requestFixture.request_body_status),
		response_body_status: bodyStatus(requestFixture.response_body_status)
	};
	const session: SessionDetail = sessionFixture;
	const message: SessionMessage = messageFixture;
	expect(request).toEqual(requestFixture);
	expect(session).toEqual(sessionFixture);
	expect(message).toEqual(messageFixture);
});
