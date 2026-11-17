// Who is signed in, for every page. The cookie itself is HttpOnly (the page never sees it);
// this holds what the server said about it, and the CSRF token for state changes.
import { get, post, signIn as exchange, useCsrf, whenSignedOut } from './api.js';

export interface SessionInfo {
	project: { id: string; slug: string; name: string };
	scopes: string[];
	csrf_token: string;
	profile: string;
	sim_controls: boolean;
}

export const session = $state<{ status: 'loading' | 'signed-out' | 'signed-in'; info: SessionInfo | null }>({
	status: 'loading',
	info: null
});

function adopt(info: SessionInfo) {
	session.info = info;
	session.status = 'signed-in';
	useCsrf(info.csrf_token);
}

whenSignedOut(() => {
	session.status = 'signed-out';
	session.info = null;
	useCsrf('');
});

export async function load(): Promise<void> {
	try {
		adopt(await get<SessionInfo>('/session/current'));
	} catch {
		session.status = 'signed-out';
	}
}

export async function signIn(key: string): Promise<void> {
	adopt(await exchange<SessionInfo>(key.trim()));
}

export async function signOut(): Promise<void> {
	try {
		await post('/session/sign-out');
	} finally {
		session.status = 'signed-out';
		session.info = null;
		useCsrf('');
	}
}
