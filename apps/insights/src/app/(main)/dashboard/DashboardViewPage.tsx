'use client';
import { Column } from '@umami/react-zen';
import { useEffect } from 'react';
import { BoardControls } from '@/app/(main)/boards/[boardId]/BoardControls';
import { BoardViewBody } from '@/app/(main)/boards/[boardId]/BoardViewBody';
import { ConsoleCrossSellBanner } from '@/app/(main)/ConsoleCrossSellBanner';
import { Empty } from '@/components/common/Empty';
import { PageBody } from '@/components/common/PageBody';
import { useBoard, useLoginQuery, useMessages, useNavigation } from '@/components/hooks';
import { DashboardProvider } from './DashboardProvider';
import { DashboardViewHeader } from './DashboardViewHeader';

function DashboardContent() {
  const { board } = useBoard();
  const { t, messages } = useMessages();
  const rows = board?.parameters?.rows ?? [];
  const hasComponents = rows.some(row => row.columns?.some(column => !!column.component));

  if (!hasComponents) {
    return <Empty message={t(messages.emptyDashboard)} />;
  }

  return <BoardViewBody />;
}

export function DashboardViewPage() {
  const { teamId, router } = useNavigation();
  const { user } = useLoginQuery();

  useEffect(() => {
    if (teamId) {
      router.replace('/dashboard');
    }
  }, [teamId, router]);

  if (teamId) {
    return null;
  }

  return (
    <DashboardProvider>
      <PageBody>
        <Column>
          <DashboardViewHeader />
          <ConsoleCrossSellBanner user={user} />
          <BoardControls />
          <DashboardContent />
        </Column>
      </PageBody>
    </DashboardProvider>
  );
}
