// Copyright 2020 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

export const transport = () => {
	return {
		dial: dial,
		listen_on: (addr) => {
			let err = new Error("Listening on WebSockets is not possible from within a browser");
			err.name = "NotSupportedError";
			throw err;
		},
	};
}

/// Turns a string multiaddress into a WebSockets string URL.
// TODO: support dns addresses as well
const multiaddr_to_ws = (addr) => {
	let parsed = addr.match(/^\/(ip4|ip6|dns4|dns6)\/(.*?)\/tcp\/(.*?)\/(ws|wss|x-parity-ws\/(.*)|x-parity-wss\/(.*))$/);
	let proto = 'wss';
	if (parsed[4] == 'ws' || parsed[4] == 'x-parity-ws') {
		proto = 'ws';
	}
	let url = decodeURIComponent(parsed[5] || parsed[6] || '');
	if (parsed != null) {
		if (parsed[1] == 'ip6') {
			return proto + "://[" + parsed[2] + "]:" + parsed[3] + url;
		} else {
			return proto + "://" + parsed[2] + ":" + parsed[3] + url;
		}
	}

	let err = new Error("Address not supported: " + addr);
	err.name = "NotSupportedError";
	throw err;
}

// Attempt to dial a multiaddress.
const dial = (addr) => {
	let ws = new WebSocket(multiaddr_to_ws(addr));
	let reader = read_queue();

	return new Promise((resolve, reject) => {
		// TODO: handle ws.onerror properly after dialing has happened
		ws.onerror = (ev) => reject(ev);
		ws.onmessage = (ev) => reader.inject_blob(ev.data);
		ws.onclose = () => reader.inject_eof();
		ws.onopen = () => resolve({
			read: (function*() { while(ws.readyState == 1) { yield reader.next(); } })(),
			write: (data) => {
				if (ws.readyState == 1) {
					ws.send(data);
					return promise_when_ws_finished(ws);
				} else {
					return Promise.reject("WebSocket is closed");
				}
			},
			shutdown: () => {},
			close: () => ws.close()
		});
	});
}

// Takes a WebSocket object and returns a Promise that resolves when bufferedAmount is 0.
const promise_when_ws_finished = (ws) => {
	if (ws.bufferedAmount == 0) {
		return Promise.resolve();
	}

	return new Promise((resolve, reject) => {
		setTimeout(function check() {
			if (ws.bufferedAmount == 0) {
				resolve();
			} else {
				setTimeout(check, 100);
			}
		}, 2);
	})
}

// Creates a queue reading system.
const read_queue = () => {
	// State of the queue.
	let state = {
		// Array of promises resolving to `ArrayBuffer`s, that haven't been transmitted back with
		// `next` yet.
		queue: new Array(),
		// If `resolve` isn't null, it is a "resolve" function of a promise that has already been
		// returned by `next`. It should be called with some data.
		resolve: null,
	};

	return {
		// Inserts a new Blob in the queue.
		inject_blob: (blob) => {
			if (state.resolve != null) {
				var resolve = state.resolve;
				state.resolve = null;

				var reader = new FileReader();
				reader.addEventListener("loadend", () => resolve(reader.result));
				reader.readAsArrayBuffer(blob);
			} else {
				state.queue.push(new Promise((resolve, reject) => {
					var reader = new FileReader();
					reader.addEventListener("loadend", () => resolve(reader.result));
					reader.readAsArrayBuffer(blob);
				}));
			}
		},

		// Inserts an EOF message in the queue.
		inject_eof: () => {
			if (state.resolve != null) {
				var resolve = state.resolve;
				state.resolve = null;
				resolve(null);
			} else {
				state.queue.push(Promise.resolve(null));
			}
		},

		// Returns a Promise that yields the next entry as an ArrayBuffer.
		next: () => {
			if (state.queue.length != 0) {
				return state.queue.shift(0);
			} else {
				if (state.resolve !== null)
					throw "Internal error: already have a pending promise";
				return new Promise((resolve, reject) => {
					state.resolve = resolve;
				});
			}
		}
	};
};
