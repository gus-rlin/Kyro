-- B120 shares the admitted B154 transport and its durable delivery receipt.
ALTER TABLE public.app_sched_calendar_connections ADD COLUMN connector_id uuid;
ALTER TABLE public.app_sched_calendar_connections ADD CONSTRAINT calendar_provider_binding
    CHECK ((provider='synthetic' AND connector_id IS NULL) OR
           (provider='google_calendar' AND connector_id IS NOT NULL));
ALTER TABLE public.app_sched_calendar_outbox ADD COLUMN connector_call_id uuid;
ALTER TABLE public.app_sched_calendar_outbox ADD CONSTRAINT calendar_connector_call
    FOREIGN KEY (tenant_id,application_id,connector_call_id)
    REFERENCES public.app_connector_calls(tenant_id,application_id,id);
ALTER TABLE public.app_sched_calendar_events ADD COLUMN active boolean NOT NULL DEFAULT true;
CREATE INDEX calendar_delivery_binding ON public.app_sched_calendar_outbox
    (tenant_id,application_id,connection_id,object_id,object_version DESC);
