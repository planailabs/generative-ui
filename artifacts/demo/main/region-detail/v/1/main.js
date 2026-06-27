// `apply` handler: the form submits { region }. Look up that region's figure and fold it into
// KV so the line below updates in place.
function handle(event, host) {
  const region = (event.value && event.value.region) || "emea";
  const figures = { emea: 48200, amer: 71800, apac: 33500 };
  const revenue = figures[region] || 0;
  host.kv.set("region", region.toUpperCase());
  // QuickJS has no Intl; format thousands separators by hand.
  host.kv.set("region_revenue", "$" + String(revenue).replace(/\B(?=(\d{3})+(?!\d))/g, ","));
  return { region, revenue };
}
