'use client';
import { Alert, AlertTitle, Button, Column, Row, Text } from '@umami/react-zen';
import { useState } from 'react';
import { getItem, setItem } from '@/lib/storage';

// Console cross-sell bridge (RYP-X1): Insights is the only Railyard surface
// with a proven self-serve funnel (anyone can add a site and watch traffic
// with no sales step). This banner turns that funnel into Console signups by
// deep-linking into Console's own `/auth/start?email=&src=insights`, which
// sends the visitor a one-time sign-in code and records first-touch
// attribution on the new org (apps/console/lib/session.ts, migration 0011).
const DISMISS_KEY = 'railyard-console-banner-dismissed-v1';
const CONSOLE_URL = (process.env.railyardConsoleUrl || '').replace(/\/+$/, '');

function emailLikeUsername(username?: string | null): string {
  if (!username) return '';
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(username) ? username : '';
}

export function ConsoleCrossSellBanner({ user }: { user?: { username?: string } }) {
  const [dismissed, setDismissed] = useState<boolean>(() => !!getItem(DISMISS_KEY));

  if (!CONSOLE_URL || dismissed) {
    return null;
  }

  const email = emailLikeUsername(user?.username);
  const params = new URLSearchParams({ src: 'insights' });
  if (email) {
    params.set('email', email);
  }
  const href = `${CONSOLE_URL}/auth/start?${params.toString()}`;

  function handleDismiss() {
    setItem(DISMISS_KEY, true);
    setDismissed(true);
  }

  return (
    <Column justifyContent="center" alignItems="center" width="100%" paddingTop="3">
      <Row width="640px">
        <Alert style={{ width: '100%' }}>
          <AlertTitle>Ship the app you are tracking, with the same account</AlertTitle>
          <Text size="2" color="muted">
            Railyard Console deploys, meters AI usage and bills this app from one dashboard. No
            separate signup: use this email and we will send a sign-in code.
          </Text>
          <Row gap="3" paddingTop="2">
            <Button variant="primary" onPress={() => window.open(href, '_self')}>
              Open Railyard Console ↗
            </Button>
            <Button onPress={handleDismiss}>Dismiss</Button>
          </Row>
        </Alert>
      </Row>
    </Column>
  );
}
