import { useCallback, useEffect, useRef, useState } from 'react';
import { listIdentical } from '../../core/ipc';
import type { IdenticalRow } from '../../core/ipc';
import type { CompareOwner } from '../../core/types/generated/CompareOwner';
import { RequestFence } from '../state/request-fence';
import { identicalResultRequestKey } from '../state/result-workspace';

const PAGE_SIZE = 300;

// Identical rows must come from the authenticated compare snapshot; this query never rescans roots.
export function useIdenticalResults(owner: CompareOwner) {
  const [searchDraft, setSearchDraft] = useState('');
  const [searchQuery, setSearchQuery] = useState('');
  const [rows, setRows] = useState<IdenticalRow[]>([]);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const requestFence = useRef(new RequestFence());

  useEffect(() => {
    const timer = window.setTimeout(() => setSearchQuery(searchDraft.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [searchDraft]);

  const loadPage = useCallback(async (offset: number) => {
    const requestKey = identicalResultRequestKey(owner, searchQuery, offset);
    const ticket = requestFence.current.start(requestKey);
    setLoading(true);
    if (offset === 0) {
      setRows([]);
      setTotal(0);
      setError('');
    }
    try {
      const page = await listIdentical(owner, searchQuery, offset, PAGE_SIZE);
      if (!requestFence.current.owns(ticket)) return;
      setLoading(false);
      setError('');
      setTotal(page.total);
      setRows((previousRows) => (offset === 0 ? page.rows : [...previousRows, ...page.rows]));
    } catch (loadError) {
      if (!requestFence.current.owns(ticket)) return;
      setLoading(false);
      setError(String(loadError));
      if (offset === 0) {
        setRows([]);
        setTotal(0);
      }
    }
  }, [owner, searchQuery]);

  useEffect(() => { void loadPage(0); }, [loadPage]);
  useEffect(() => () => requestFence.current.invalidate(), []);

  return {
    searchDraft,
    setSearchDraft,
    rows,
    total,
    error,
    loading,
    loadMore: () => loadPage(rows.length),
  };
}
