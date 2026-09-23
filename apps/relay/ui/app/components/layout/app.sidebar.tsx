import * as React from "react";
import {
  Inferences,
  Episodes,
  Functions,
  SupervisedFineTuning,
  Dataset,
  GridCheck,
  SequenceChecks,
  Playground,
  Model,
  Chat,
  Cost,
  SidebarCollapse,
  SidebarExpand,
} from "~/components/icons/Icons";
import {
  Activity,
  Download,
  FileCode2,
  Gauge,
  Inbox,
  KeyRound,
  LayoutGrid,
  Plus,
  Receipt,
} from "lucide-react";
import {
  Sidebar,
  SidebarContent,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuItem,
  SidebarMenuButton,
  SidebarRail,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupLabel,
  SidebarGroupContent,
  useSidebar,
} from "~/components/ui/sidebar";
import { useActivePath } from "~/hooks/use-active-path";
import { useAutopilotAvailable } from "~/context/autopilot-available";
import { useConfig } from "~/context/config";
import { RelayLogo } from "~/components/icons/Icons";
import { Link } from "react-router";
import type { IconProps } from "~/components/icons/Icons";
import TensorZeroStatusIndicator from "./TensorZeroStatusIndicator";
import { ReadOnlyBadge } from "./ReadOnlyBadge";

interface NavigationItem {
  title: string;
  url: string;
  icon: React.FC<IconProps>;
}

interface NavigationSection {
  title: string;
  items: NavigationItem[];
}

const navigation: NavigationSection[] = [
  {
    title: "Observability",
    items: [
      {
        title: "Inferences",
        url: "/observability/inferences",
        icon: Inferences,
      },
      {
        title: "Episodes",
        url: "/observability/episodes",
        icon: Episodes,
      },
      {
        title: "Functions",
        url: "/observability/functions",
        icon: Functions,
      },
      {
        title: "Models",
        url: "/observability/models",
        icon: Model,
      },
      {
        title: "Model Catalog",
        url: "/models",
        icon: LayoutGrid,
      },
      {
        title: "Provider Health",
        url: "/providers",
        icon: Activity,
      },
    ],
  },
  {
    title: "Billing",
    items: [
      {
        title: "Billing",
        url: "/billing",
        icon: Cost,
      },
      {
        title: "Settlements",
        url: "/billing/settlements",
        icon: Receipt,
      },
      {
        title: "API Key Spend",
        url: "/billing/keys",
        icon: KeyRound,
      },
      {
        title: "Spend Caps",
        url: "/billing/budgets",
        icon: Gauge,
      },
      {
        title: "Cost Events",
        url: "/billing/events",
        icon: Inbox,
      },
      {
        title: "Usage Export",
        url: "/usage/export",
        icon: Download,
      },
    ],
  },
  {
    title: "Evaluations",
    items: [
      {
        title: "Inference Evaluations",
        url: "/evaluations",
        icon: GridCheck,
      },
      {
        title: "Workflow Evaluations",
        url: "/workflow-evaluations",
        icon: SequenceChecks,
      },
    ],
  },
  {
    title: "Optimization",
    items: [
      {
        title: "Supervised Fine-Tuning",
        url: "/optimization/supervised-fine-tuning",
        icon: SupervisedFineTuning,
      },
    ],
  },
  {
    title: "Resources",
    items: [
      {
        title: "Playground",
        url: "/playground",
        icon: Playground,
      },
      {
        title: "Datasets",
        url: "/datasets",
        icon: Dataset,
      },
      {
        title: "API Keys",
        url: "/api-keys",
        icon: KeyRound,
      },
    ],
  },
];

export function AppSidebar({ ...props }: React.ComponentProps<typeof Sidebar>) {
  const { state, toggleSidebar } = useSidebar();
  const activePathUtils = useActivePath();
  const autopilotAvailable = useAutopilotAvailable();
  const config = useConfig();

  return (
    <Sidebar collapsible="icon" {...props}>
      <SidebarHeader className="pb-4">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              className="text-black hover:bg-transparent focus-visible:bg-transparent active:bg-transparent"
            >
              <Link to="/" className="flex items-center gap-2">
                <RelayLogo size={16} />
                <span className="font-semibold whitespace-nowrap transition-opacity duration-200 group-data-[collapsible=icon]:opacity-0">
                  Relay
                </span>
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent className="overflow-x-hidden! overflow-y-auto! transition-[width] duration-200">
        <SidebarGroup>
          <SidebarGroupContent className="flex flex-col gap-1">
            <SidebarMenuItem className="list-none">
              <SidebarMenuButton
                asChild
                tooltip={state === "collapsed" ? "Overview" : undefined}
                isActive={activePathUtils.isActive("/")}
              >
                <Link to="/" className="flex items-center gap-2">
                  <LayoutGrid className="h-4 w-4" />
                  <span className="whitespace-nowrap transition-opacity duration-200 group-data-[collapsible=icon]:opacity-0">
                    Overview
                  </span>
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
            {autopilotAvailable && (
              <SidebarMenuItem className="relative list-none">
                <SidebarMenuButton
                  asChild
                  tooltip={state === "collapsed" ? "Autopilot" : undefined}
                  isActive={activePathUtils.isActive("/autopilot")}
                >
                  <Link to="/autopilot" className="flex items-center gap-2">
                    <Chat className="h-4 w-4" />
                    <span className="whitespace-nowrap transition-opacity duration-200 group-data-[collapsible=icon]:opacity-0">
                      Autopilot
                    </span>
                  </Link>
                </SidebarMenuButton>
                {state === "expanded" && (
                  <Link
                    to="/autopilot/sessions/new"
                    className="text-fg-muted hover:text-fg-primary absolute top-1/2 right-2 z-10 -translate-y-1/2 rounded p-0.5 transition-colors"
                    aria-label="New session"
                  >
                    <Plus className="h-4 w-4" />
                  </Link>
                )}
              </SidebarMenuItem>
            )}
            {config.config_in_database && (
              <SidebarMenuItem className="list-none">
                <SidebarMenuButton
                  asChild
                  tooltip={state === "collapsed" ? "Configuration" : undefined}
                  isActive={activePathUtils.isActive("/config")}
                >
                  <Link to="/config" className="flex items-center gap-2">
                    <FileCode2 className="h-4 w-4" />
                    <span className="whitespace-nowrap transition-opacity duration-200 group-data-[collapsible=icon]:opacity-0">
                      Configuration
                    </span>
                  </Link>
                </SidebarMenuButton>
              </SidebarMenuItem>
            )}
          </SidebarGroupContent>
        </SidebarGroup>
        {navigation.map((section) => (
          <SidebarGroup key={section.title}>
            <SidebarGroupLabel>{section.title}</SidebarGroupLabel>
            <SidebarGroupContent className="flex flex-col gap-1">
              {section.items?.map((item) => (
                <SidebarMenuItem key={item.title} className="list-none">
                  <SidebarMenuButton
                    asChild
                    tooltip={state === "collapsed" ? item.title : undefined}
                    isActive={activePathUtils.isActive(item.url)}
                  >
                    <Link to={item.url} className="flex items-center gap-2">
                      <item.icon className="h-4 w-4" />
                      <span className="whitespace-nowrap transition-opacity duration-200 group-data-[collapsible=icon]:opacity-0">
                        {item.title}
                      </span>
                    </Link>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              ))}
            </SidebarGroupContent>
          </SidebarGroup>
        ))}
      </SidebarContent>
      <SidebarFooter className="relative">
        <ReadOnlyBadge />
        <TensorZeroStatusIndicator collapsed={state === "collapsed"} />
        <SidebarMenuItem className="list-none">
          <SidebarMenuButton
            aria-label="Toggle sidebar"
            className="w-auto cursor-pointer"
            tooltip={state === "collapsed" ? "Toggle sidebar" : undefined}
            onClick={toggleSidebar}
          >
            {state === "expanded" ? (
              <SidebarCollapse className="h-4 w-4" />
            ) : (
              <SidebarExpand className="h-4 w-4" />
            )}
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  );
}
