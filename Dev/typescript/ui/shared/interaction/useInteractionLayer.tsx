import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  type ReactNode,
  type RefObject,
} from 'react';
import {
  interactionCommandFromKey,
  orderedInteractionLayers,
  resolveInteractionCommand,
  type InteractionCommand,
  type InteractionLayerEntry,
  type InteractionLayerKind,
} from '#ui/shared/interaction/interactionLayers.ts';

interface InteractionLayerHandlers {
  dismiss?: () => void;
  find?: () => void;
  compare?: () => void;
  synchronize?: () => void;
  zoom_in?: () => void;
  zoom_out?: () => void;
  zoom_reset?: () => void;
}

interface InteractionLayerRegistration {
  id: symbol;
  parentId: symbol | null;
  kind: InteractionLayerKind;
  handler(command: InteractionCommand): (() => void) | undefined;
  root: HTMLElement | null;
}

interface InteractionLayerAuthority {
  register(registration: InteractionLayerRegistration): () => void;
  isTopLayer(layerId: symbol): boolean;
}

const InteractionLayerAuthorityContext = createContext<InteractionLayerAuthority | null>(null);
const ParentInteractionLayerContext = createContext<symbol | null>(null);

function setElementInactive(element: HTMLElement, inactive: boolean) {
  element.inert = inactive;
  if (inactive) element.setAttribute('aria-hidden', 'true');
  else element.removeAttribute('aria-hidden');
}

export function InteractionLayerProvider({ applicationRoot, children }: {
  applicationRoot: HTMLElement;
  children: ReactNode;
}) {
  const layers = useRef<InteractionLayerEntry[]>([]);
  const nextOrder = useRef(0);

  const synchronizeModality = useCallback(() => {
    const modalLayers = orderedInteractionLayers(layers.current.filter((layer) => layer.kind === 'modal'));
    const topModalId = modalLayers[0]?.id ?? null;
    setElementInactive(applicationRoot, topModalId !== null);
    for (const layer of modalLayers) {
      if (layer.root) setElementInactive(layer.root, layer.id !== topModalId);
    }
  }, [applicationRoot]);

  const register = useCallback((registration: InteractionLayerRegistration) => {
    nextOrder.current += 1;
    const entry: InteractionLayerEntry = { ...registration, order: nextOrder.current };
    layers.current.push(entry);
    synchronizeModality();
    return () => {
      const index = layers.current.findIndex((candidate) => candidate.id === registration.id);
      if (index < 0) return;
      const [removed] = layers.current.splice(index, 1);
      if (removed?.root) setElementInactive(removed.root, false);
      synchronizeModality();
    };
  }, [synchronizeModality]);

  const isTopLayer = useCallback((layerId: symbol) => (
    orderedInteractionLayers(layers.current)[0]?.id === layerId
  ), []);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      const command = interactionCommandFromKey(event);
      if (!command) return;
      const resolution = resolveInteractionCommand(command, layers.current);
      if (resolution.disposition === 'unhandled') return;
      event.preventDefault();
      event.stopPropagation();
      if (resolution.disposition === 'handled') resolution.run();
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, []);

  useEffect(() => () => {
    for (const layer of layers.current) {
      if (layer.root) setElementInactive(layer.root, false);
    }
    layers.current = [];
    setElementInactive(applicationRoot, false);
  }, [applicationRoot]);

  return (
    <InteractionLayerAuthorityContext.Provider value={{ register, isTopLayer }}>
      {children}
    </InteractionLayerAuthorityContext.Provider>
  );
}

export function useInteractionLayer({ active = true, kind, rootRef, handlers }: {
  active?: boolean;
  kind: InteractionLayerKind;
  rootRef?: RefObject<HTMLElement | null>;
  handlers: InteractionLayerHandlers;
}): { layerId: symbol; isTopLayer: () => boolean } {
  const authority = useContext(InteractionLayerAuthorityContext);
  if (!authority) throw new Error('useInteractionLayer requires InteractionLayerProvider');
  const parentId = useContext(ParentInteractionLayerContext);
  const layerId = useRef(Symbol(kind)).current;
  const currentHandlers = useRef(handlers);
  currentHandlers.current = handlers;

  useLayoutEffect(() => {
    if (!active) return undefined;
    return authority.register({
      id: layerId,
      parentId,
      kind,
      root: rootRef?.current ?? null,
      handler: (command) => currentHandlers.current[command],
    });
  }, [active, authority, kind, layerId, parentId, rootRef]);

  const isTopLayer = useCallback(
    () => authority.isTopLayer(layerId),
    [authority, layerId],
  );

  return {
    layerId,
    isTopLayer,
  };
}

export function InteractionLayerScope({ layerId, children }: {
  layerId: symbol;
  children: ReactNode;
}) {
  return (
    <ParentInteractionLayerContext.Provider value={layerId}>
      {children}
    </ParentInteractionLayerContext.Provider>
  );
}
