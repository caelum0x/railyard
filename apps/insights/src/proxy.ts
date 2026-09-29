// Railyard fork addition: with INSIGHTS_API_ONLY=1 only the Railyard API and
// the heartbeat are served (src/railyard/api-only.ts); the upstream dashboard
// stays internal.
import { NextResponse, type NextRequest } from 'next/server';
import { allowedPath } from '@/railyard/api-only';

export function proxy(request: NextRequest) {
  if (!allowedPath(request.nextUrl.pathname, process.env.INSIGHTS_API_ONLY === '1')) {
    return new NextResponse('Not found', { status: 404 });
  }
  return NextResponse.next();
}

export default proxy;
